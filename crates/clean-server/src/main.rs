//! `clean-server` — the reference Clean host for HTTP applications.
//!
//! Owns the HTTP surface and delegates everything else to `clean-host-core`
//! (§1.11). See `PLAN.md` for the build order and `host.wit` for the contract.

mod config;
mod guest;
mod listener;
mod routing;
mod startup;

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
                    runtime.router.routes().len(),
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
    let runtime = Arc::new(runtime);

    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    tokio_rt.block_on(async {
        let serving = Arc::clone(&runtime);
        listener::serve(serving, shutdown_signal()).await
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

/// SIGTERM (supervisors) or Ctrl-C (interactive) begins a graceful drain.
///
/// SIGHUP-triggered reload is Phase 4; until then it is deliberately not
/// handled rather than silently ignored under a different meaning.
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
