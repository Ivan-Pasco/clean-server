//! Shared harness for the end-to-end suites.
//!
//! Boots the real `clean-server` binary against the real acceptance guest and
//! drives it over real sockets — no in-process shortcuts, because the things
//! being tested (TLS handshake, protocol selection, upgrade) only exist on the
//! wire.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

pub fn binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("clean-server")
}

/// The acceptance guest, or None when it has not been built.
pub fn guest_wasm() -> Option<PathBuf> {
    let guest = repo_root().join("testing/fake-guest/guest.wasm");
    if guest.exists() {
        Some(guest)
    } else {
        eprintln!(
            "skipping: {} not built (run testing/fake-guest/build.sh)",
            guest.display()
        );
        None
    }
}

/// Ask the OS for a free port. A fixed port makes the suite fail whenever
/// anything else is listening.
pub fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A server process, killed when the test ends however it ends.
pub struct Server {
    child: Child,
    port: u16,
    /// Kept alive so the config file outlives the process.
    _dir: tempfile::TempDir,
    /// Set for TLS servers.
    ca: Option<Vec<u8>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    pub fn start() -> Option<Self> {
        Self::start_with("")
    }

    /// Start with an extra `[server]` block.
    pub fn start_with(server_block: &str) -> Option<Self> {
        Self::start_full("instances-min = 1\ninstances-max = 4", server_block)
    }

    /// Start with explicit `[runtime]` and `[server]` blocks.
    ///
    /// The acceptance guest imports the composition-test bridge, so unless the
    /// caller supplies its own `[bridges]` block one is added — otherwise
    /// SRVH-01 would refuse startup for every suite.
    pub fn start_full(runtime_block: &str, server_block: &str) -> Option<Self> {
        let guest = guest_wasm()?;
        let port = free_port();
        let bridge = repo_root().join("testing/fake-bridge/bridge.wasm");
        let bridges = if server_block.contains("[bridges]") || !bridge.exists() {
            String::new()
        } else {
            format!(
                "[bridges]\n\"clean:fake-bridge/store\" = \"{}\"\n",
                bridge.display()
            )
        };

        // `listen` must come from us, so strip any the caller supplied and
        // append ours after their keys.
        let server_block = if server_block.trim().is_empty() {
            "[server]".to_string()
        } else {
            server_block.to_string()
        };

        let config = format!(
            r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "acceptance"
wasm  = "{}"
world = "clean:host/server@0.1"

[runtime]
{runtime_block}

{bridges}
{server_block}
listen = "127.0.0.1:{port}"
"#,
            guest.display()
        );

        Some(Self::spawn(config, port, None))
    }

    /// Start with the composition-test bridge composed.
    ///
    /// The acceptance guest imports `clean:fake-bridge/store`, so every server
    /// that runs it needs this bridge configured or SRVH-01 refuses startup.
    pub fn start_composed() -> Option<Self> {
        let bridge = repo_root().join("testing/fake-bridge/bridge.wasm");
        if !bridge.exists() {
            eprintln!("skipping: run testing/fake-bridge/build.sh");
            return None;
        }
        Self::start_full(
            "instances-min = 1\ninstances-max = 4",
            &format!(
                "[bridges]\n\"clean:fake-bridge/store\" = \"{}\"\n\n[server]",
                bridge.display()
            ),
        )
    }

    /// Start with TLS enabled, using a freshly generated self-signed cert.
    pub fn start_tls() -> Option<Self> {
        let guest = guest_wasm()?;
        let port = free_port();

        let bridge_path = repo_root().join("testing/fake-bridge/bridge.wasm");
        let bridges = if bridge_path.exists() {
            format!(
                "[bridges]\n\"clean:fake-bridge/store\" = \"{}\"\n",
                bridge_path.display()
            )
        } else {
            String::new()
        };

        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("server.crt");
        let key_path = dir.path().join("server.key");
        std::fs::write(&cert_path, cert.cert.pem()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
        let ca_der = cert.cert.der().to_vec();

        let config = format!(
            r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "acceptance"
wasm  = "{}"
world = "clean:host/server@0.1"

[runtime]
instances-min = 1
instances-max = 4

{bridges}
[server]
listen   = "127.0.0.1:{port}"
tls      = "tls"
tls-cert = "{}"
tls-key  = "{}"
"#,
            guest.display(),
            cert_path.display(),
            key_path.display()
        );

        let mut server = Self::spawn_in(config, port, dir, Some(ca_der));
        server.wait_until_listening();
        Some(server)
    }

    fn spawn(config: String, port: u16, ca: Option<Vec<u8>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Self::spawn_in(config, port, dir, ca);
        server.wait_until_listening();
        server
    }

    fn spawn_in(config: String, port: u16, dir: tempfile::TempDir, ca: Option<Vec<u8>>) -> Self {
        let config_path = dir.path().join("host.toml");
        std::fs::write(&config_path, config).unwrap();

        let child = Command::new(binary())
            .arg(&config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("clean-server binary should be built");

        Self {
            child,
            port,
            _dir: dir,
            ca,
        }
    }

    fn wait_until_listening(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                let mut stderr = String::new();
                if let Some(mut e) = self.child.stderr.take() {
                    let _ = e.read_to_string(&mut stderr);
                }
                panic!("server exited early ({status}):\n{stderr}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not start listening on port {}", self.port);
    }

    pub fn addr(&self) -> (&'static str, u16) {
        ("127.0.0.1", self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Issue a plaintext request; returns (status, headers, body).
    pub fn request(&self, method: &str, path: &str) -> (u16, Vec<(String, String)>, String) {
        self.request_with_body(method, path, b"")
    }

    pub fn request_with_body(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> (u16, Vec<(String, String)>, String) {
        self.request_with_headers(method, path, body, &[])
    }

    /// Issue a request with extra headers.
    pub fn request_with_headers(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> (u16, Vec<(String, String)>, String) {
        let mut stream = TcpStream::connect(self.addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();

        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n")
                .into_bytes();
        for (name, value) in headers {
            request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if !body.is_empty() {
            request.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(body);

        stream.write_all(&request).unwrap();
        stream.flush().unwrap();
        read_response(stream)
    }

    /// Perform a WebSocket handshake and return the response head.
    pub fn websocket_handshake(&self, key: &str) -> (u16, Vec<(String, String)>, String) {
        let mut stream = TcpStream::connect(self.addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(
                format!(
                    "GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                     Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
                     Sec-WebSocket-Key: {key}\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let (status, headers) = read_head(&mut reader);
        // A 101 has no body; anything buffered is already frame data.
        (status, headers, String::new())
    }

    /// Complete a handshake and hand back the raw stream for frame reading.
    pub fn websocket_connect(&self) -> TcpStream {
        let mut stream = TcpStream::connect(self.addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
                  Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        stream.flush().unwrap();

        // Consume exactly the response head, leaving frames in the stream.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            let n = stream.read(&mut byte).expect("handshake response");
            if n == 0 {
                panic!("connection closed during handshake");
            }
            head.push(byte[0]);
        }
        assert!(
            String::from_utf8_lossy(&head).contains("101"),
            "handshake failed: {}",
            String::from_utf8_lossy(&head)
        );
        stream
    }

    /// A TLS client configured to trust this server's self-signed cert.
    fn tls_config(&self, alpn: &[&str]) -> Arc<rustls::ClientConfig> {
        let ca = self.ca.as_ref().expect("TLS server");
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(ca.clone()))
            .unwrap();

        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|p| p.as_bytes().to_vec()).collect();
        Arc::new(config)
    }

    fn tls_stream(
        &self,
        alpn: &[&str],
    ) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
        let config = self.tls_config(alpn);
        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let conn = rustls::ClientConnection::new(config, server_name).unwrap();
        let sock = TcpStream::connect(self.addr()).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        rustls::StreamOwned::new(conn, sock)
    }

    /// GET over TLS using HTTP/1.1, returning the body.
    pub fn tls_get(&self, path: &str) -> String {
        let mut tls = self.tls_stream(&["http/1.1"]);
        tls.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        tls.flush().unwrap();

        let mut raw = Vec::new();
        let _ = tls.read_to_end(&mut raw);
        let text = String::from_utf8_lossy(&raw);
        text.split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default()
    }

    /// The protocol ALPN settled on for the given client offer.
    pub fn tls_alpn(&self, offer: &[&str]) -> Option<String> {
        let mut tls = self.tls_stream(offer);
        // Drive the handshake to completion.
        tls.flush().unwrap();
        let _ = tls.conn.complete_io(&mut tls.sock);
        tls.conn
            .alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned())
    }
}

fn read_head<R: BufRead>(reader: &mut R) -> (u16, Vec<(String, String)>) {
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("malformed status line: {status_line:?}"))
        .parse()
        .unwrap();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    (status, headers)
}

fn read_response(stream: TcpStream) -> (u16, Vec<(String, String)>, String) {
    let mut reader = BufReader::new(stream);
    let (status, headers) = read_head(&mut reader);

    let mut body = String::new();
    let _ = reader.read_to_string(&mut body);

    // `Connection: close` means the body runs to EOF, but a chunked response
    // still arrives framed; decode it so callers see the payload either way.
    let is_chunked = headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    if is_chunked {
        body = dechunk(&body);
    }

    (status, headers, body)
}

/// Decode `Transfer-Encoding: chunked` framing.
fn dechunk(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some((size_line, tail)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        if tail.len() < size {
            out.push_str(tail);
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].strip_prefix("\r\n").unwrap_or(&tail[size..]);
    }
    out
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}
