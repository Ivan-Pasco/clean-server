//! Process entry point for the `clean-server` host.
//!
//! Lives in the library rather than in `main.rs` because two binaries start
//! this host: `clean-server` (the host's own CLI) and `clean-runtime`
//! (`--world=server`, Manager §00.13). Both must boot, drain, and handle
//! signals identically — a second copy of this logic in the runtime crate
//! would be free to drift, and the difference would only show up in
//! production shutdown behavior.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use crate::{admin, listener, reload, startup};

/// Boot the host against `config_path` and serve until shutdown.
///
/// With `check`, validate the configuration and the guest's imports, report,
/// and exit without binding a listener.
pub fn run(config_path: &Path, check: bool) -> ExitCode {
    init_logging();

    if check {
        return match startup::boot(config_path) {
            Ok(runtime) => {
                println!(
                    "ok: {} route(s) registered; would listen on {}",
                    runtime.router.len(),
                    runtime.server.listen
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                report(&e);
                ExitCode::FAILURE
            }
        };
    }

    match serve(config_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report(&e);
            ExitCode::FAILURE
        }
    }
}

fn serve(config_path: &Path) -> anyhow::Result<()> {
    let runtime = startup::boot(config_path)?;
    let drain = runtime.server.reload_drain_timeout;
    let admin_listen = runtime.server.admin_listen;
    let deployment_mode = runtime.deployment_mode;
    let runtime = Arc::new(runtime);

    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    tokio_rt.block_on(async {
        // One shutdown signal, observed by every listener.
        let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);

        // SIGHUP → reload (§1.10). Runs for the life of the process.
        {
            let runtime = Arc::clone(&runtime);
            let mut stop = stop_tx.subscribe();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop.recv() => break,
                        got = hangup() => {
                            if !got { break; }
                            tracing::info!(target: "clean_server", "SIGHUP received; reloading");
                            let response = admin::apply(
                                &runtime,
                                &reload::Request::ReloadGuest { guest: None },
                            );
                            if !response.is_ok() {
                                // CLNH-53 keeps the old composition serving, so
                                // this is a warning rather than a fatal error.
                                tracing::warn!(
                                    target: "clean_server",
                                    response = %response.to_json(),
                                    "reload did not complete"
                                );
                            }
                        }
                    }
                }
            });
        }

        // The local dev socket, when the deployment mode allows it.
        #[cfg(unix)]
        let dev_socket = if admin::dev_socket_enabled(deployment_mode) {
            let runtime = Arc::clone(&runtime);
            let mut stop = stop_tx.subscribe();
            let path = admin::dev_socket_path();
            let cleanup = path.clone();
            tokio::spawn(async move {
                let shutdown = async move {
                    let _ = stop.recv().await;
                };
                if let Err(e) = admin::serve_dev_socket(runtime, path, shutdown).await {
                    tracing::warn!(target: "clean_server::admin", error = %e, "dev socket unavailable");
                }
            });
            Some(cleanup)
        } else {
            None
        };

        // The admin API, when configured.
        if let Some(addr) = admin_listen {
            let runtime = Arc::clone(&runtime);
            let mut stop = stop_tx.subscribe();
            tokio::spawn(async move {
                let shutdown = async move {
                    let _ = stop.recv().await;
                };
                if let Err(e) = listener::serve_admin(runtime, addr, shutdown).await {
                    tracing::error!(target: "clean_server::admin", error = %e, "admin API stopped");
                }
            });
        }

        let serving = Arc::clone(&runtime);
        let result = listener::serve(serving, shutdown_signal()).await;
        // Wind down the auxiliary listeners with the main one.
        let _ = stop_tx.send(());

        // Remove the socket file here rather than trusting the task to get
        // there first: the process exits as soon as this function returns, so
        // a task that has not yet observed the broadcast would leave the file
        // behind for the next start to trip over.
        #[cfg(unix)]
        if let Some(path) = dev_socket {
            let _ = std::fs::remove_file(&path);
        }

        result
    })?;

    // §1.10 / CLNH-56: stop accepting, drain in-flight work, then drop.
    tracing::info!(
        target: "clean_server",
        drain_secs = drain.as_secs(),
        "draining in-flight requests"
    );

    match Arc::try_unwrap(runtime) {
        Ok(runtime) => {
            if let Err(e) = runtime.host.shutdown(drain) {
                // A drain timeout is worth reporting but does not make the
                // process exit non-zero — the shutdown itself succeeded.
                tracing::warn!(target: "clean_server", error = %e, "shutdown incomplete");
            }
        }
        Err(_) => {
            tracing::warn!(
                target: "clean_server",
                "connections still hold the runtime; skipping graceful drain"
            );
        }
    }

    tracing::info!(target: "clean_server", "stopped");
    Ok(())
}

/// Wait for one SIGHUP. Returns false when the handler cannot be installed,
/// which ends the reload loop rather than spinning on an error.
///
/// SIGHUP is the supervisor's reload verb — `systemctl reload`, `launchctl
/// kickstart` — and is deliberately distinct from SIGTERM's drain.
async fn hangup() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::hangup()) {
            Ok(mut sig) => sig.recv().await.is_some(),
            Err(e) => {
                tracing::error!(target: "clean_server", error = %e, "cannot install SIGHUP handler");
                false
            }
        }
    }

    #[cfg(not(unix))]
    {
        // No SIGHUP on Windows; reload arrives via the admin API instead.
        std::future::pending::<()>().await;
        false
    }
}

/// SIGTERM (supervisors) or Ctrl-C (interactive) begins a graceful drain.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(target: "clean_server", error = %e, "cannot install SIGTERM handler");
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => tracing::info!(target: "clean_server", "SIGTERM received"),
            _ = tokio::signal::ctrl_c() => tracing::info!(target: "clean_server", "interrupt received"),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!(target: "clean_server", "interrupt received");
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_env("CLEAN_SERVER_LOG")
        .unwrap_or_else(|_| EnvFilter::new("clean_server=info,warn"));

    // Diagnostics go to stderr, not stdout. `tracing_subscriber::fmt` defaults
    // to stdout, which would interleave log lines with `--check`'s
    // machine-readable output and defeat the `2>` redirection every supervisor
    // config uses to separate the two.
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Print an error with its full cause chain.
///
/// CH-05 makes startup failures loud; a bare top-line message that hides the
/// cause defeats that.
fn report(error: &anyhow::Error) {
    eprintln!("error: {error}");
    for cause in error.chain().skip(1) {
        eprintln!("  caused by: {cause}");
    }
}
