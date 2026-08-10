//! TLS termination (§1.7).
//!
//! `rustls` only. The deployment shape (§1.2) promises no shared-object
//! dependencies beyond libc, which `native-tls`/OpenSSL would break.
//!
//! ALPN advertises `h2` before `http/1.1` when `[server] h2` is on, so an
//! HTTP/2-capable client negotiates it during the handshake and the listener
//! learns which protocol to speak from the completed connection rather than by
//! sniffing bytes.

use std::io;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig as RustlsConfig;

use crate::config::{ServerConfig, TlsMode};

/// ALPN identifiers, most preferred first.
const ALPN_H2: &[u8] = b"h2";
const ALPN_HTTP11: &[u8] = b"http/1.1";

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("cannot read {what} `{path}`: {source}")]
    Read {
        what: &'static str,
        path: String,
        source: io::Error,
    },
    #[error("`{path}` contains no {what}")]
    Empty { what: &'static str, path: String },
    #[error("invalid TLS configuration: {0}")]
    Invalid(String),
}

/// Build a rustls acceptor from `[server] tls-cert` / `tls-key`.
///
/// Returns `None` when TLS is off, so the caller serves plaintext.
pub fn acceptor(config: &ServerConfig) -> Result<Option<tokio_rustls::TlsAcceptor>, TlsError> {
    if config.tls == TlsMode::None {
        return Ok(None);
    }

    if config.tls == TlsMode::StartTls {
        // Declaring support we do not have would be worse than refusing: a
        // deployment would believe it was protected.
        return Err(TlsError::Invalid(
            "`tls = \"starttls\"` is not implemented; use \"tls\" for direct TLS".into(),
        ));
    }

    // config.rs guarantees both are present whenever tls != none.
    let cert_path = config
        .tls_cert
        .as_ref()
        .ok_or_else(|| TlsError::Invalid("tls-cert is required".into()))?;
    let key_path = config
        .tls_key
        .as_ref()
        .ok_or_else(|| TlsError::Invalid("tls-key is required".into()))?;

    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut rustls_config = RustlsConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| {
            TlsError::Invalid(format!("certificate and key do not form a valid pair: {e}"))
        })?;

    rustls_config.alpn_protocols = if config.h2 {
        vec![ALPN_H2.to_vec(), ALPN_HTTP11.to_vec()]
    } else {
        vec![ALPN_HTTP11.to_vec()]
    };

    Ok(Some(tokio_rustls::TlsAcceptor::from(Arc::new(
        rustls_config,
    ))))
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let data = std::fs::read(path).map_err(|source| TlsError::Read {
        what: "TLS certificate",
        path: path.display().to_string(),
        source,
    })?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut data.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|e| TlsError::Invalid(format!("{}: {e}", path.display())))?;

    if certs.is_empty() {
        return Err(TlsError::Empty {
            what: "PEM certificates",
            path: path.display().to_string(),
        });
    }
    Ok(certs)
}

fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let data = std::fs::read(path).map_err(|source| TlsError::Read {
        what: "TLS private key",
        path: path.display().to_string(),
        source,
    })?;

    // Accept any of the three PEM key encodings rather than forcing operators
    // to convert; which one a tool emits is not their choice to make.
    rustls_pemfile::private_key(&mut data.as_slice())
        .map_err(|e| TlsError::Invalid(format!("{}: {e}", path.display())))?
        .ok_or_else(|| TlsError::Empty {
            what: "a PEM private key",
            path: path.display().to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clean_host_core::HostConfig;

    fn server_config(block: &str) -> ServerConfig {
        let text = format!(
            r#"
[host]
name = "clean-server"
version = "0.1.0"
component-model = "0.3.0"
deployment-mode = "development"

[guest]
name = "app"
wasm = "./app.wasm"
world = "clean:host/server@0.1"

{block}
"#
        );
        let host = HostConfig::parse(&text, "/srv/host.toml").unwrap();
        ServerConfig::from_host_config(&host).unwrap()
    }

    /// `TlsAcceptor` has no `Debug`, so `unwrap_err` is unavailable.
    fn expect_err(result: Result<Option<tokio_rustls::TlsAcceptor>, TlsError>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        }
    }

    /// A self-signed cert/key pair written to a temp dir.
    fn cert_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("server.crt");
        let key_path = dir.path().join("server.key");
        std::fs::write(&cert_path, cert.cert.pem()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
        (dir, cert_path, key_path)
    }

    #[test]
    fn tls_off_produces_no_acceptor() {
        let config = server_config("");
        assert!(acceptor(&config).unwrap().is_none());
    }

    #[test]
    fn a_valid_pair_builds_an_acceptor() {
        let (_dir, cert, key) = cert_pair();
        let config = server_config(&format!(
            "[server]\ntls = \"tls\"\ntls-cert = \"{}\"\ntls-key = \"{}\"",
            cert.display(),
            key.display()
        ));
        assert!(acceptor(&config).unwrap().is_some());
    }

    #[test]
    fn a_missing_certificate_file_names_the_path() {
        let (_dir, _cert, key) = cert_pair();
        let config = server_config(&format!(
            "[server]\ntls = \"tls\"\ntls-cert = \"/nope/missing.crt\"\ntls-key = \"{}\"",
            key.display()
        ));
        let err = expect_err(acceptor(&config));
        assert!(err.contains("missing.crt"), "{err}");
    }

    #[test]
    fn a_certificate_file_without_pem_content_is_rejected() {
        let (dir, _cert, key) = cert_pair();
        let empty = dir.path().join("empty.crt");
        std::fs::write(&empty, "not a certificate").unwrap();

        let config = server_config(&format!(
            "[server]\ntls = \"tls\"\ntls-cert = \"{}\"\ntls-key = \"{}\"",
            empty.display(),
            key.display()
        ));
        let err = expect_err(acceptor(&config));
        assert!(err.contains("no PEM certificates"), "{err}");
    }

    #[test]
    fn starttls_is_refused_rather_than_silently_serving_plaintext() {
        let (_dir, cert, key) = cert_pair();
        let config = server_config(&format!(
            "[server]\ntls = \"starttls\"\ntls-cert = \"{}\"\ntls-key = \"{}\"",
            cert.display(),
            key.display()
        ));
        let err = expect_err(acceptor(&config));
        assert!(err.contains("not implemented"), "{err}");
    }

    #[test]
    fn mismatched_cert_and_key_are_rejected() {
        let (_dir_a, cert, _key_a) = cert_pair();
        let (_dir_b, _cert_b, other_key) = cert_pair();

        let config = server_config(&format!(
            "[server]\ntls = \"tls\"\ntls-cert = \"{}\"\ntls-key = \"{}\"",
            cert.display(),
            other_key.display()
        ));
        assert!(acceptor(&config).is_err());
    }
}
