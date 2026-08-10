//! M0 acceptance, automated (PLAN.md Layer B).
//!
//! Boots the real binary against `testing/fixtures/hello-world/host.toml` and
//! drives it with a real HTTP client. This is the test that proves the whole
//! path — config, composition, route registration, request marshaling, guest
//! invocation, response marshaling — actually works end to end.
//!
//! The guest is `testing/fake-guest/guest.wasm`, built by its `build.sh` from
//! a world that imports the same interfaces `host.wit` declares.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/clean-server.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn binary() -> PathBuf {
    // The integration-test binary sits in target/<profile>/deps.
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("clean-server")
}

/// A server process that is killed when the test ends, however it ends.
struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Start the server on an ephemeral port and wait for it to accept.
    fn start() -> Option<Self> {
        let root = repo_root();
        let guest = root.join("testing/fake-guest/guest.wasm");
        if !guest.exists() {
            // The guest is a build artifact of testing/fake-guest/build.sh,
            // which needs wasm-tools. Skip rather than fail: a developer
            // without wasm-tools should still be able to run the unit tests.
            eprintln!(
                "skipping: {} not built (run testing/fake-guest/build.sh)",
                guest.display()
            );
            return None;
        }

        // Ask the OS for a free port, then release it. A fixed port makes the
        // suite fail whenever anything else is listening on 3000.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("host.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "hello-world"
wasm  = "{}"
world = "clean:host/server@0.1"

[runtime]
instances-min = 1
instances-max = 4

[server]
listen = "127.0.0.1:{port}"
"#,
                guest.display()
            ),
        )
        .unwrap();

        let child = Command::new(binary())
            .arg(&config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("clean-server binary should be built");

        let server = Server { child, port };
        server.wait_until_listening();
        // The tempdir must outlive startup, which has already read the config.
        drop(dir);
        Some(server)
    }

    fn wait_until_listening(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not start listening on port {}", self.port);
    }

    /// Issue a request and return (status line, headers, body).
    fn request(&self, method: &str, path: &str) -> (u16, Vec<(String, String)>, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .write_all(
                format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .expect("status line has a code")
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

        let mut body = String::new();
        let _ = reader.read_to_string(&mut body);
        (status, headers, body)
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

#[test]
fn m0_acceptance_get_root_returns_hello_world() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, body) = server.request("GET", "/");

    assert_eq!(status, 200);
    assert_eq!(body, "hello world");
    assert_eq!(
        header(&headers, "content-type"),
        Some("text/plain; charset=utf-8")
    );
}

#[test]
fn unregistered_path_is_404() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, _, _) = server.request("GET", "/nothing-here");
    assert_eq!(status, 404);
}

#[test]
fn wrong_method_is_405_with_an_allow_header() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, _) = server.request("POST", "/");
    assert_eq!(status, 405);
    assert_eq!(header(&headers, "allow"), Some("GET"));
}

#[test]
fn head_returns_headers_without_a_body() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, body) = server.request("HEAD", "/");
    assert_eq!(status, 200);
    assert!(body.is_empty(), "HEAD must not carry a body, got {body:?}");
    // The length still describes what a GET would return.
    assert_eq!(header(&headers, "content-length"), Some("11"));
}

#[test]
fn the_same_instance_serves_repeated_requests_cleanly() {
    // Instances are reset and returned to the pool between requests
    // (CLNH-29). If reset were broken, a later response would differ.
    let Some(server) = Server::start() else {
        return;
    };
    for _ in 0..10 {
        let (status, _, body) = server.request("GET", "/");
        assert_eq!(status, 200);
        assert_eq!(body, "hello world");
    }
}

#[test]
fn missing_guest_wasm_fails_startup_with_a_pointed_error() {
    // §8 question #7: a missing guest is a startup error naming the config key
    // and the resolved path — never a silent fallback (CH-05).
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("host.toml");
    std::fs::write(
        &config_path,
        r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "missing"
wasm  = "./does-not-exist.wasm"
world = "clean:host/server@0.1"
"#,
    )
    .unwrap();

    let output = Command::new(binary())
        .arg("--check")
        .arg(&config_path)
        .output()
        .expect("binary runs");

    assert!(!output.status.success(), "startup should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[guest] wasm"), "{stderr}");
    assert!(stderr.contains("does-not-exist.wasm"), "{stderr}");
}

#[test]
fn a_configured_bridge_is_refused_rather_than_ignored() {
    // Bridge composition is Phase 3. Until then a [bridges] entry must fail
    // loudly — silently ignoring it would mean a guest importing that
    // capability fails much later, in a much more confusing place (CH-05).
    let root = repo_root();
    let guest = root.join("testing/fake-guest/guest.wasm");
    if !guest.exists() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("host.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[host]
name            = "clean-server"
version         = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name  = "hello-world"
wasm  = "{}"
world = "clean:host/server@0.1"

[bridges]
"clean:session/store" = "./session.wasm"
"#,
            guest.display()
        ),
    )
    .unwrap();

    let output = Command::new(binary())
        .arg("--check")
        .arg(&config_path)
        .output()
        .expect("binary runs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Phase 3"), "{stderr}");
}

#[test]
fn hcv_06_parity_holds_between_host_wit_and_the_linker() {
    // The same check CI runs (§16.14). Declared interfaces must match
    // registered ones exactly, in both directions.
    let output = Command::new(binary())
        .arg("parity")
        .arg("--wit")
        .arg(repo_root().join("host.wit"))
        .output()
        .expect("binary runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "HCV-06 parity failed:\n{stdout}");
}
