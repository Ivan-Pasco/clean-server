//! The `[server]` block (§1.6).
//!
//! Schema source of truth:
//! `foundation/02 components/hosts/clean-server/schema/server-block.toml.md`.
//!
//! Everything else in `host.toml` — `[host]`, `[guest]`, `[runtime]`,
//! `[bridges]` — belongs to `clean-host-core` (CLNH-11) and is parsed there.
//! This module reads only the block clean-server owns.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clean_host_core::{DeploymentMode, HostConfig};
use serde::Deserialize;

/// TLS mode for the listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    None,
    StartTls,
    Tls,
}

/// Whether cookies get the `Secure` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieSecure {
    /// Derived from whether TLS is active.
    Auto,
    Always,
    Never,
}

/// The whole `[server]` block is parsed and validated at startup even though
/// M0 only consumes part of it. Validating a key the listener does not read yet
/// is the point: a typo in `tls-cert` should fail on the release that
/// introduces it, not on the one that finally reads it.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub tls: TlsMode,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub h2: bool,
    pub h1_keep_alive: Duration,
    pub body_max_bytes: u64,
    pub request_timeout: Duration,
    pub queue_depth: u32,
    pub mount: String,
    pub cookie_samesite: String,
    pub cookie_secure: CookieSecure,
    pub socket_queue_max: u64,
    pub admin_listen: Option<SocketAddr>,
    /// Bearer token for the admin API (SRVH-08). Required whenever
    /// `admin-listen` is set.
    pub admin_auth_bearer: Option<String>,
    pub health_path: String,
    pub metrics_path: Option<String>,
    pub reload_drain_timeout: Duration,
    pub trust_proxy_headers: bool,
    pub allow_plaintext: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("[server] {0}")]
pub struct ServerConfigError(String);

impl ServerConfig {
    /// Read the `[server]` block out of an already-parsed `host.toml`.
    ///
    /// An absent block is fine — every key has a documented default except
    /// `listen`, which falls back to the loopback address M0 targets.
    pub fn from_host_config(host: &HostConfig) -> Result<Self, ServerConfigError> {
        let raw: RawServer = match host.host_block("server") {
            Some(value) => value
                .clone()
                .try_into()
                .map_err(|e| ServerConfigError(format!("{e}")))?,
            None => RawServer::default(),
        };

        let listen = raw
            .listen
            .as_deref()
            .unwrap_or("127.0.0.1:3000")
            .parse()
            .map_err(|e| {
                ServerConfigError(format!(
                    "listen: `{}` is not a valid host:port address ({e})",
                    raw.listen.as_deref().unwrap_or("127.0.0.1:3000")
                ))
            })?;

        let tls = match raw.tls.as_deref() {
            None | Some("none") => TlsMode::None,
            Some("starttls") => TlsMode::StartTls,
            Some("tls") => TlsMode::Tls,
            Some(other) => {
                return Err(ServerConfigError(format!(
                    "tls: expected \"none\", \"starttls\", or \"tls\"; got `{other}`"
                )))
            }
        };

        // Conditional requirement from the schema: certs are mandatory as soon
        // as TLS is on. Catching it here means a misconfigured deployment fails
        // at startup rather than on the first HTTPS connection.
        if tls != TlsMode::None {
            if raw.tls_cert.is_none() {
                return Err(ServerConfigError(
                    "tls-cert is required when tls is not \"none\"".into(),
                ));
            }
            if raw.tls_key.is_none() {
                return Err(ServerConfigError(
                    "tls-key is required when tls is not \"none\"".into(),
                ));
            }
        }

        let cookie_secure = match raw.cookie_secure.as_deref() {
            None | Some("auto") => CookieSecure::Auto,
            Some("always") | Some("true") => CookieSecure::Always,
            Some("never") | Some("false") => CookieSecure::Never,
            Some(other) => {
                return Err(ServerConfigError(format!(
                    "cookie-secure: expected \"auto\", \"always\", or \"never\"; got `{other}`"
                )))
            }
        };

        let admin_listen = match &raw.admin_listen {
            None => None,
            Some(s) => Some(s.parse().map_err(|e| {
                ServerConfigError(format!("admin-listen: `{s}` is not a valid address ({e})"))
            })?),
        };

        // SRVH-08: the admin API is network-reachable and authenticated. An
        // unauthenticated reload endpoint is a remote restart button, so the
        // server refuses to start rather than exposing one.
        let admin_auth_bearer = raw.admin_auth.as_ref().and_then(|a| a.bearer.clone());
        if admin_listen.is_some() && admin_auth_bearer.is_none() {
            return Err(ServerConfigError(
                "admin-listen is set but no admin token is configured.\n                   Add `[server.admin-auth] bearer = \"<token>\"`, or remove `admin-listen` \
                 to disable the admin API (SRVH-08)."
                    .into(),
            ));
        }
        if let Some(token) = &admin_auth_bearer {
            if token.len() < 16 {
                return Err(ServerConfigError(
                    concat!(
                        "the admin bearer token is shorter than 16 characters; ",
                        "it guards a remote reload endpoint"
                    )
                    .into(),
                ));
            }
        }

        let config = Self {
            listen,
            tls,
            tls_cert: raw.tls_cert.map(PathBuf::from),
            tls_key: raw.tls_key.map(PathBuf::from),
            h2: raw.h2.unwrap_or(true),
            h1_keep_alive: duration(raw.h1_keep_alive.as_deref(), "h1-keep-alive", 60)?,
            body_max_bytes: size(
                raw.body_max_bytes.as_deref(),
                "body-max-bytes",
                10 * 1024 * 1024,
            )?,
            request_timeout: duration(raw.request_timeout.as_deref(), "request-timeout", 30)?,
            queue_depth: raw.queue_depth.unwrap_or(256),
            mount: raw.mount.unwrap_or_else(|| "/".to_string()),
            cookie_samesite: raw.cookie_samesite.unwrap_or_else(|| "lax".to_string()),
            cookie_secure,
            socket_queue_max: size(
                raw.socket_queue_max.as_deref(),
                "socket-queue-max",
                1024 * 1024,
            )?,
            admin_listen,
            admin_auth_bearer,
            health_path: raw.health_path.unwrap_or_else(|| "/_health".to_string()),
            // The schema uses an empty string to disable metrics; represent
            // that as None so no caller can accidentally route to "".
            metrics_path: raw.metrics_path.filter(|p| !p.is_empty()),
            reload_drain_timeout: duration(
                raw.reload_drain_timeout.as_deref(),
                "reload-drain-timeout",
                30,
            )?,
            trust_proxy_headers: raw.trust_proxy_headers.unwrap_or(false),
            allow_plaintext: raw.allow_plaintext.unwrap_or(false),
        };

        config.check_production_safety(host)?;
        Ok(config)
    }

    /// §1.7: plaintext in production is opt-in and loud.
    fn check_production_safety(&self, host: &HostConfig) -> Result<(), ServerConfigError> {
        if host.host.deployment_mode == DeploymentMode::Production
            && self.tls == TlsMode::None
            && !self.trust_proxy_headers
            && !self.allow_plaintext
        {
            return Err(ServerConfigError(
                "refusing to serve plaintext HTTP in production.\n  \
                 Set `[server] tls`, or `[server] trust-proxy-headers = true` if a reverse proxy \
                 terminates TLS, or `[server] allow-plaintext = true` to accept the risk \
                 explicitly (`cln doctor` flags it)."
                    .into(),
            ));
        }
        Ok(())
    }

    /// Whether cookies should carry `Secure`, resolving `auto` against TLS
    /// status. A trusted proxy terminating TLS counts as secure.
    ///
    /// Used by the session envelope in Phase 3; the resolution rule is tested
    /// now so the security default cannot regress unnoticed before then.
    #[allow(dead_code)]
    pub fn cookies_are_secure(&self) -> bool {
        match self.cookie_secure {
            CookieSecure::Always => true,
            CookieSecure::Never => false,
            CookieSecure::Auto => self.tls != TlsMode::None || self.trust_proxy_headers,
        }
    }
}

fn duration(
    value: Option<&str>,
    key: &str,
    default_secs: u64,
) -> Result<Duration, ServerConfigError> {
    match value {
        None => Ok(Duration::from_secs(default_secs)),
        Some(s) => parse_duration(s).map_err(|e| ServerConfigError(format!("{key}: {e}"))),
    }
}

fn size(value: Option<&str>, key: &str, default: u64) -> Result<u64, ServerConfigError> {
    match value {
        None => Ok(default),
        Some(s) => parse_size(s).map_err(|e| ServerConfigError(format!("{key}: {e}"))),
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".into());
    }
    let (digits, mult) = match s.chars().last().unwrap().to_ascii_uppercase() {
        'K' => (&s[..s.len() - 1], 1024),
        'M' => (&s[..s.len() - 1], 1024 * 1024),
        'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("`{s}` is not a valid size (expected e.g. `10M`)"))?
        .checked_mul(mult)
        .ok_or_else(|| format!("size `{s}` overflows"))
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let invalid = || format!("`{s}` is not a valid duration (expected e.g. `30s`, `10ms`)");

    let (digits, unit) = if let Some(d) = s.strip_suffix("ms") {
        (d, "ms")
    } else if let Some(d) = s.strip_suffix('s') {
        (d, "s")
    } else if let Some(d) = s.strip_suffix('m') {
        (d, "m")
    } else if let Some(d) = s.strip_suffix('h') {
        (d, "h")
    } else {
        return Err(invalid());
    };

    let n: u64 = digits.trim().parse().map_err(|_| invalid())?;
    Ok(match unit {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        _ => Duration::from_secs(n * 3600),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawAdminAuth {
    bearer: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct RawServer {
    listen: Option<String>,
    tls: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    h2: Option<bool>,
    h1_keep_alive: Option<String>,
    body_max_bytes: Option<String>,
    request_timeout: Option<String>,
    queue_depth: Option<u32>,
    mount: Option<String>,
    cookie_samesite: Option<String>,
    cookie_secure: Option<String>,
    socket_queue_max: Option<String>,
    admin_listen: Option<String>,
    admin_auth: Option<RawAdminAuth>,
    health_path: Option<String>,
    metrics_path: Option<String>,
    reload_drain_timeout: Option<String>,
    trust_proxy_headers: Option<bool>,
    allow_plaintext: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_config(server_block: &str) -> HostConfig {
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

{server_block}
"#
        );
        HostConfig::parse(&text, "/srv/host.toml").unwrap()
    }

    #[test]
    fn absent_server_block_uses_documented_defaults() {
        let cfg = ServerConfig::from_host_config(&host_config("")).unwrap();
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:3000");
        assert_eq!(cfg.tls, TlsMode::None);
        assert!(cfg.h2);
        assert_eq!(cfg.body_max_bytes, 10 * 1024 * 1024);
        assert_eq!(cfg.request_timeout, Duration::from_secs(30));
        assert_eq!(cfg.queue_depth, 256);
        assert_eq!(cfg.mount, "/");
        assert_eq!(cfg.health_path, "/_health");
        assert_eq!(cfg.cookie_samesite, "lax");
        assert!(cfg.metrics_path.is_none());
        assert_eq!(cfg.reload_drain_timeout, Duration::from_secs(30));
    }

    #[test]
    fn full_block_parses_every_documented_key() {
        let cfg = ServerConfig::from_host_config(&host_config(
            r#"
[server]
listen = "0.0.0.0:8080"
tls = "tls"
tls-cert = "/etc/clean/server.crt"
tls-key = "/etc/clean/server.key"
h2 = false
h1-keep-alive = "45s"
body-max-bytes = "2M"
request-timeout = "15s"
queue-depth = 512
mount = "/api"
cookie-samesite = "strict"
cookie-secure = "always"
socket-queue-max = "2M"
admin-listen = "127.0.0.1:9091"
health-path = "/healthz"
metrics-path = "/metrics"
reload-drain-timeout = "10s"
trust-proxy-headers = true
allow-plaintext = true

[server.admin-auth]
bearer = "0123456789abcdef"
"#,
        ))
        .unwrap();

        assert_eq!(cfg.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(cfg.tls, TlsMode::Tls);
        assert!(!cfg.h2);
        assert_eq!(cfg.h1_keep_alive, Duration::from_secs(45));
        assert_eq!(cfg.body_max_bytes, 2 * 1024 * 1024);
        assert_eq!(cfg.queue_depth, 512);
        assert_eq!(cfg.mount, "/api");
        assert_eq!(cfg.cookie_samesite, "strict");
        assert_eq!(cfg.socket_queue_max, 2 * 1024 * 1024);
        assert_eq!(cfg.admin_listen.unwrap().to_string(), "127.0.0.1:9091");
        assert_eq!(cfg.metrics_path.as_deref(), Some("/metrics"));
    }

    #[test]
    fn tls_without_cert_or_key_is_rejected() {
        let err = ServerConfig::from_host_config(&host_config("[server]\ntls = \"tls\""))
            .unwrap_err()
            .to_string();
        assert!(err.contains("tls-cert is required"), "{err}");
    }

    #[test]
    fn empty_metrics_path_disables_metrics() {
        let cfg =
            ServerConfig::from_host_config(&host_config("[server]\nmetrics-path = \"\"")).unwrap();
        assert!(cfg.metrics_path.is_none());
    }

    #[test]
    fn an_admin_listener_without_a_token_is_refused() {
        // SRVH-08: an unauthenticated admin API is a remote restart button.
        let err = ServerConfig::from_host_config(&host_config(
            "[server]\nadmin-listen = \"127.0.0.1:9091\"",
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("admin token"), "{err}");
        assert!(err.contains("SRVH-08"), "{err}");
    }

    #[test]
    fn an_admin_listener_with_a_token_is_accepted() {
        let cfg = ServerConfig::from_host_config(&host_config(
            "[server]\nadmin-listen = \"127.0.0.1:9091\"\n\n[server.admin-auth]\nbearer = \"0123456789abcdef\"",
        ))
        .unwrap();
        assert_eq!(cfg.admin_auth_bearer.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn a_trivially_short_admin_token_is_refused() {
        let err = ServerConfig::from_host_config(&host_config(
            "[server]\nadmin-listen = \"127.0.0.1:9091\"\n\n[server.admin-auth]\nbearer = \"short\"",
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("16 characters"), "{err}");
    }

    #[test]
    fn no_admin_listener_needs_no_token() {
        let cfg = ServerConfig::from_host_config(&host_config("")).unwrap();
        assert!(cfg.admin_listen.is_none());
        assert!(cfg.admin_auth_bearer.is_none());
    }

    #[test]
    fn bad_listen_address_names_the_key() {
        let err =
            ServerConfig::from_host_config(&host_config("[server]\nlisten = \"not-an-addr\""))
                .unwrap_err()
                .to_string();
        assert!(err.contains("listen"), "{err}");
    }

    #[test]
    fn production_plaintext_is_refused_unless_opted_in() {
        let text = r#"
[host]
name = "clean-server"
version = "0.1.0"
component-model = "0.3.0"
deployment-mode = "production"

[guest]
name = "app"
wasm = "./app.wasm"
world = "clean:host/server@0.1"
"#;
        let host = HostConfig::parse(text, "/srv/host.toml").unwrap();
        let err = ServerConfig::from_host_config(&host)
            .unwrap_err()
            .to_string();
        assert!(err.contains("plaintext"), "{err}");

        // Any one of the three escape hatches makes it start.
        let with_optin = HostConfig::parse(
            &format!("{text}\n[server]\nallow-plaintext = true"),
            "/srv/host.toml",
        )
        .unwrap();
        assert!(ServerConfig::from_host_config(&with_optin).is_ok());

        let with_proxy = HostConfig::parse(
            &format!("{text}\n[server]\ntrust-proxy-headers = true"),
            "/srv/host.toml",
        )
        .unwrap();
        assert!(ServerConfig::from_host_config(&with_proxy).is_ok());
    }

    #[test]
    fn development_plaintext_is_fine() {
        assert!(ServerConfig::from_host_config(&host_config("")).is_ok());
    }

    #[test]
    fn cookie_secure_auto_follows_tls_and_proxy() {
        let plain = ServerConfig::from_host_config(&host_config("")).unwrap();
        assert!(!plain.cookies_are_secure());

        let proxied =
            ServerConfig::from_host_config(&host_config("[server]\ntrust-proxy-headers = true"))
                .unwrap();
        assert!(proxied.cookies_are_secure());

        let forced =
            ServerConfig::from_host_config(&host_config("[server]\ncookie-secure = \"always\""))
                .unwrap();
        assert!(forced.cookies_are_secure());
    }
}
