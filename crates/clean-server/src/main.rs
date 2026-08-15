//! `clean-server` — the reference Clean host for HTTP applications.
//!
//! Owns the HTTP surface and delegates everything else to `clean-host-core`
//! (§1.11). See `PLAN.md` for the build order and `host.wit` for the contract.
//!
//! This binary is the host's own CLI. The same host is also reachable as
//! `clean-runtime --world=server` (Manager §00.13); both call
//! `entrypoint::run`, so their boot and drain behavior cannot drift.

use clean_server::{conformance, entrypoint, guest};

use std::path::PathBuf;
use std::process::ExitCode;

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

    /// Run the CMOD-03 host conformance suite (Platform 15 §10.1).
    ///
    /// The shipping gate: a host that fails conformance must not ship. Exits
    /// non-zero unless every check ran and passed — a skipped check is not a
    /// pass.
    Conformance {
        #[arg(long, default_value = "host.wit")]
        wit: PathBuf,
        /// Where the canonical Clean-compiled components live.
        #[arg(long, default_value = "tests/cln/conformance")]
        corpus: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match &cli.command {
        Some(Command::Parity { wit }) => return run_parity(wit),
        Some(Command::Conformance { wit, corpus }) => return run_conformance(wit, corpus),
        None => {}
    }

    let Some(config_path) = cli.config.clone() else {
        eprintln!("error: a host.toml path is required\n\nusage: clean-server <CONFIG>");
        return ExitCode::FAILURE;
    };

    entrypoint::run(&config_path, cli.check)
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

/// CMOD-03: the shipping gate.
fn run_conformance(wit: &std::path::Path, corpus: &std::path::Path) -> ExitCode {
    let report = conformance::run(wit, &guest::registered_interfaces(), corpus);
    print!("{}", report.render());

    if report.conforms() {
        ExitCode::SUCCESS
    } else {
        // Non-zero for both FAILED and INCOMPLETE. A gate that exits zero on a
        // partial run is not a gate.
        ExitCode::FAILURE
    }
}
