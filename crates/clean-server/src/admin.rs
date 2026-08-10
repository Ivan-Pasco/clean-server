//! The reload triggers (§1.10): admin HTTP API and the local dev socket.
//!
//! Both carry the same wire protocol but have deliberately different trust
//! models, and they are kept in separate functions so those models cannot be
//! accidentally shared:
//!
//! - **Admin HTTP API** — network-reachable, so it requires a bearer token
//!   (SRVH-08) and refuses to start without one. Bound to loopback by default.
//! - **Local dev socket** — a Unix socket whose access control *is* filesystem
//!   permissions (SRVH-08 explicitly forbids requiring auth here). Created
//!   0600 so only the owning user can speak to it.

use std::sync::Arc;

use clean_host_core::DeploymentMode;

use crate::reload::{policy_refusal, Request, Response};
use crate::startup::Runtime;

/// Apply a reload request to the running host.
///
/// Shared by both transports so the two cannot drift in what an op means.
/// SRVH-07: every op is answered — there is no fire-and-forget path.
pub fn apply(runtime: &Arc<Runtime>, request: &Request) -> Response {
    let started = std::time::Instant::now();
    let op = request.op();

    if let Some(reason) = policy_refusal(request, runtime.deployment_mode) {
        tracing::info!(target: "clean_server::admin", op, reason = %reason, "request refused");
        return Response::refused(op, reason);
    }

    match request {
        Request::ReloadGuest { guest } => {
            if let Some(path) = guest {
                // Reloading a different path would need the config rewritten,
                // which is the manager's job under `cln run` (PLAN.md §8 #3).
                // Accepting and ignoring it would silently reload the wrong
                // artifact.
                return Response::error(
                    op,
                    format!(
                        "reloading a different guest path is not supported; \
                         `[guest] wasm` names the artifact (requested: {path})"
                    ),
                );
            }
            do_reload(runtime, op, started)
        }
        Request::ReloadChain => do_reload(runtime, op, started),
        // Refused by policy above; unreachable in practice.
        Request::SwapMiddleware { .. } => {
            Response::refused(op, "swap-middleware is not implemented")
        }
    }
}

fn do_reload(runtime: &Arc<Runtime>, op: &str, started: std::time::Instant) -> Response {
    let in_flight_before = runtime
        .host
        .health()
        .pool
        .map(|p| p.checkouts_active)
        .unwrap_or(0);

    match runtime.host.reload() {
        Ok(()) => {
            for warning in runtime.host.warnings() {
                tracing::warn!(target: "clean_server::admin", "{warning}");
            }
            tracing::info!(
                target: "clean_server::admin",
                op,
                duration_ms = started.elapsed().as_millis() as u64,
                "reload complete"
            );
            Response::ok(op, started, in_flight_before)
        }
        Err(e) => {
            // CLNH-53: the previous composition is still serving. Say so, or an
            // operator reading only the error assumes the process is down.
            tracing::error!(
                target: "clean_server::admin",
                op,
                error = %e,
                "reload failed; the previous composition is still active"
            );
            Response::error(op, format!("{e}\n(the previous guest is still serving)"))
        }
    }
}

/// Handle one raw request body, from either transport.
pub fn handle_body(runtime: &Arc<Runtime>, body: &str) -> Response {
    match Request::parse(body) {
        Ok(request) => apply(runtime, &request),
        Err(e) => Response::error(e.op.as_deref().unwrap_or("unknown"), e.message),
    }
}

/// Whether a bearer token matches the configured one.
///
/// Constant-time, for the same reason CSRF validation is: a byte-by-byte early
/// exit leaks the token to anyone who can time the response.
pub fn token_matches(configured: &str, presented: &str) -> bool {
    if configured.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in configured.bytes().zip(presented.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Extract a bearer token from an `Authorization` header value.
pub fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token.trim())
    } else {
        None
    }
}

/// The dev socket path for this process (§1.10).
///
/// `$XDG_RUNTIME_DIR/clean-server-<pid>.sock`, falling back to the temp dir
/// where that is unset — macOS does not define it.
#[cfg(unix)]
pub fn dev_socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("clean-server-{}.sock", std::process::id()))
}

/// Serve the local dev socket until `shutdown` resolves.
///
/// SRVH-08: no authentication. Access control is the socket's filesystem
/// permissions, which is why the file is created 0600 and removed on exit.
#[cfg(unix)]
pub async fn serve_dev_socket(
    runtime: Arc<Runtime>,
    path: std::path::PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    // A socket left behind by a crashed process would make bind fail.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)
        .map_err(|e| anyhow::anyhow!("cannot bind dev socket at {}: {e}", path.display()))?;

    // Owner-only. Without this the socket inherits umask, which on a shared
    // machine can mean any local user can trigger a reload.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
        anyhow::anyhow!(
            "cannot restrict dev socket permissions on {}: {e}",
            path.display()
        )
    })?;

    tracing::info!(target: "clean_server::admin", socket = %path.display(), "dev socket listening");

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let runtime = Arc::clone(&runtime);

                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();

                    // Newline-delimited: one request per line, one response per
                    // request, for as long as the caller keeps the socket open.
                    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                        let response = handle_body(&runtime, line.trim());
                        let mut out = response.to_json();
                        out.push('\n');
                        if reader.get_mut().write_all(out.as_bytes()).await.is_err() {
                            break;
                        }
                        line.clear();
                    }
                });
            }
        }
    }

    let _ = std::fs::remove_file(&path);
    tracing::info!(target: "clean_server::admin", "dev socket closed");
    Ok(())
}

/// Whether the dev socket should be started.
///
/// Off in production: §1.10 gates it on dev-mode, and an unauthenticated
/// reload trigger has no business existing in a production process.
pub fn dev_socket_enabled(mode: DeploymentMode) -> bool {
    mode != DeploymentMode::Production
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_token_is_accepted() {
        assert!(token_matches("s3cret", "s3cret"));
    }

    #[test]
    fn a_wrong_token_is_rejected() {
        assert!(!token_matches("s3cret", "wrong!"));
    }

    #[test]
    fn a_token_prefix_is_rejected() {
        // Guards the constant-time comparison against a length-only check.
        assert!(!token_matches("s3cret", "s3c"));
        assert!(!token_matches("s3c", "s3cret"));
    }

    #[test]
    fn a_bearer_header_yields_its_token() {
        assert_eq!(bearer_token("Bearer abc123"), Some("abc123"));
        assert_eq!(bearer_token("bearer abc123"), Some("abc123"));
    }

    #[test]
    fn a_non_bearer_scheme_is_ignored() {
        assert_eq!(bearer_token("Basic dXNlcjpwYXNz"), None);
        assert_eq!(bearer_token("abc123"), None);
    }

    #[test]
    fn the_dev_socket_is_off_in_production() {
        assert!(!dev_socket_enabled(DeploymentMode::Production));
        assert!(dev_socket_enabled(DeploymentMode::Development));
        assert!(dev_socket_enabled(DeploymentMode::Staging));
    }

    #[cfg(unix)]
    #[test]
    fn the_dev_socket_path_carries_the_pid() {
        let path = dev_socket_path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("clean-server-"), "{name}");
        assert!(name.ends_with(".sock"), "{name}");
        assert!(name.contains(&std::process::id().to_string()), "{name}");
    }
}
