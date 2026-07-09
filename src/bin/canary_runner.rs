//! canary_runner — Layer 2 canary executor for the clean-server host.
//!
//! Loads a canary `.wasm` produced by the compiler's canary corpus, instantiates
//! it against the **real** `clean_server::bridge::create_linker` (the same
//! linker the production server binary uses), invokes the module's entry
//! point, and lets any host-side `print!` / `println!` calls escape to this
//! process's stdout.  Exits 0 on clean completion, non-zero on any failure
//! (bad file, LinkError, trap, missing entry).
//!
//! The outer orchestrator (`scripts/run_canaries.sh`) is responsible for
//! diffing this process's captured stdout against the canary's
//! `// Expected output:` header.  This binary intentionally does no diffing —
//! it only exercises the host's real import set so that any `LinkError` at
//! instantiation surfaces loudly.
//!
//! Usage:
//!   canary_runner <path/to/canary.wasm>
//!
//! Exit codes:
//!   0  — module loaded, instantiated, and the entry point returned cleanly.
//!   1  — usage error or file I/O failure.
//!   2  — WASM parse / compile failure.
//!   3  — linker instantiation failure (missing import → LinkError).
//!   4  — no recognized entry export (`main` / `_start` / `start` / `init`).
//!   5  — runtime trap while executing the entry.

use std::process::ExitCode;

use clean_server::bridge::create_linker;
use clean_server::router::create_shared_router;
use clean_server::wasm::WasmState;
use wasmtime::{Engine, Module, Store};

const ENTRY_NAMES: &[&str] = &["main", "_start", "start", "init"];

// A multi-thread runtime is required because several server-side bridge
// closures call `tokio::task::block_in_place` (crypto, DB, jobs, mailer) which
// only works inside a multi-thread runtime.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    run()
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <wasm-file>", args[0]);
        return ExitCode::from(1);
    }
    let wasm_path = &args[1];

    let wasm_bytes = match std::fs::read(wasm_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("canary_runner: failed to read {}: {}", wasm_path, e);
            return ExitCode::from(1);
        }
    };

    let engine = Engine::default();

    let module = match Module::new(&engine, &wasm_bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("canary_runner: WASM parse/compile failed for {}: {}", wasm_path, e);
            return ExitCode::from(2);
        }
    };

    let linker = match create_linker(&engine) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("canary_runner: create_linker failed: {}", e);
            return ExitCode::from(3);
        }
    };

    let router = create_shared_router();
    let state = WasmState::new(router);
    let mut store = Store::new(&engine, state);

    let instance = match linker.instantiate(&mut store, &module) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("canary_runner: instantiate failed (LinkError likely): {}", e);
            return ExitCode::from(3);
        }
    };

    for name in ENTRY_NAMES {
        if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, name) {
            return match func.call(&mut store, ()) {
                Ok(()) => ExitCode::from(0),
                Err(e) => {
                    eprintln!("canary_runner: trap while executing '{}': {}", name, e);
                    ExitCode::from(5)
                }
            };
        }
    }

    eprintln!(
        "canary_runner: no recognized entry export ({}) in {}",
        ENTRY_NAMES.join("|"),
        wasm_path
    );
    ExitCode::from(4)
}
