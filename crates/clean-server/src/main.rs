//! `clean-server` — the reference Clean host for HTTP applications.
//!
//! Owns the HTTP surface and delegates everything else to `clean-host-core`
//! (§1.11). See `PLAN.md` for the build order and `host.wit` for the contract.

mod admin;
mod config;
mod envelope;
mod guest;
mod listener;
mod reload;
mod routing;
mod sockets;
mod startup;
mod tls;
mod websocket;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

/// The CLI surface (§3). `cln run` invokes this.
#[derive(Parser)]
#[command(
    name = "clean-server",
    version,
    about = "The reference Clean host for HTTP applications",
    // The common form is `clean-server host.toml`; subcommands are auxiliary.
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// Path to host.toml.
    ///
    /// §8 question #2: a positional argument, no search-path magic. When
    /// invoked through `cln run`, the manager extracts the bundled config and
    /// passes its path here.
    #[arg(value_name = "CONFIG")]
    config: Option<PathBuf>,

    /// Validate the config and the guest's imports, then exit without binding
    /// a listener.
    #[arg(long, requires = "config")]
    check: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Verify `host.wit` against the interfaces this binary registers (HCV-06).
    ///
    /// Run from CI on every commit. Exits non-zero on any parity violation.
    Parity {
        /// Path to host.wit. Defaults to the repo-root file (HCV-02).
        #[arg(long, default_value = "host.wit")]
        wit: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(Command::Parity { wit }) = &cli.command {
        return run_parity(wit);
    }

    let Some(config_path) = cli.config.clone() else {
        eprintln!("error: a host.toml path is required\n\nusage: clean-server <CONFIG>");
        return ExitCode::FAILURE;
    };

    init_logging();

    if cli.check {
        return match startup::boot(&config_path) {
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

    match run(config_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report(&e);
            ExitCode::FAILURE
        }
    }
}

fn run(config_path: PathBuf) -> anyhow::Result<()> {
    let runtime = startup::boot(&config_path)?;
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

/// HCV-06: `host.wit` must exist, parse, and match what the linker registers.
fn run_parity(wit: &std::path::Path) -> ExitCode {
    let report = clean_host_core::parity::check(wit, &guest::registered_interfaces());
    print!("{}", report.render());

    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_env("CLEAN_SERVER_LOG")
        .unwrap_or_else(|_| EnvFilter::new("clean_server=info,warn"));

    let _ = fmt().with_env_filter(filter).with_target(true).try_init();
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
