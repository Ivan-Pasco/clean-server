//! Bridge Contract Test
//!
//! Verifies that every function and alias defined in `function-registry.toml`
//! is resolvable in the server linker.  Unlike `bridge_compliance_test.rs`
//! (which stops at the first failure), this test collects **all** missing
//! registrations and reports them together, making it easier to spot
//! systematic omissions (e.g. an entire alias group missing).
//!
//! This is the load-bearing test for dual-name registration: a new bridge
//! function added without its dot-notation alias will be caught here before
//! it causes a production `LinkError`.
//!
//! See `foundation/platform-architecture/HOST_BRIDGE.md § Dual Naming`.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::path::PathBuf;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

// ---------------------------------------------------------------------------
// Registry types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct Registry {
    #[allow(dead_code)]
    meta: RegistryMeta,
    functions: Vec<FunctionEntry>,
}

#[derive(serde::Deserialize)]
struct RegistryMeta {
    #[allow(dead_code)]
    version: String,
    #[allow(dead_code)]
    generated_from: Vec<String>,
}

#[derive(serde::Deserialize)]
struct FunctionEntry {
    name: String,
    layer: u32,
    #[allow(dead_code)]
    category: String,
    module: String,
    params: Vec<String>,
    returns: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[allow(dead_code)]
    description: String,
    // Empty = unrestricted (backward compat with pre-2026-06 entries).
    // See registry header "HOST SCOPING" section.
    #[serde(default)]
    hosts: Vec<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expand_param_type(t: &str) -> Vec<&str> {
    match t {
        "string"  => vec!["i32", "i32"],
        "integer" => vec!["i64"],
        "number"  => vec!["f64"],
        "boolean" => vec!["i32"],
        "i32"     => vec!["i32"],
        "i64"     => vec!["i64"],
        "any"     => vec!["i32"],
        other => panic!("Unknown param type in registry: '{}'", other),
    }
}

fn expand_return_type(t: &str) -> Option<&str> {
    match t {
        "void"    => None,
        "ptr"     => Some("i32"),
        "string"  => Some("i32"),  // string return = ptr to length-prefixed string
        "i32"     => Some("i32"),
        "i64"     => Some("i64"),
        "boolean" => Some("i32"),
        "integer" => Some("i64"),
        "number"  => Some("f64"),
        "any"     => Some("i32"),
        other => panic!("Unknown return type in registry: '{}'", other),
    }
}

fn single_import_wat(module: &str, name: &str, params: &[String], returns: &str) -> String {
    let mut wat = format!("(module\n  (import \"{}\" \"{}\" (func", module, name);
    let wasm_params: Vec<&str> = params.iter().flat_map(|t| expand_param_type(t)).collect();
    if !wasm_params.is_empty() {
        wat.push_str(" (param");
        for p in &wasm_params {
            wat.push_str(&format!(" {}", p));
        }
        wat.push(')');
    }
    if let Some(ret) = expand_return_type(returns) {
        wat.push_str(&format!(" (result {})", ret));
    }
    wat.push_str("))\n)\n");
    wat
}

fn make_store(engine: &Engine) -> Store<WasmState> {
    let router = Arc::new(Router::new());
    Store::new(engine, WasmState::new(router))
}

// ---------------------------------------------------------------------------
// The contract test
// ---------------------------------------------------------------------------

/// Verify that every canonical function and every alias in `function-registry.toml`
/// is registered in the server linker.
///
/// Each function/alias is probed individually so **all** missing registrations
/// are collected and reported in one failure message, rather than stopping at
/// the first absent import.
///
/// This is the enforcement gate for the dual-name registration convention
/// (`register_bridge_fn!` macro).  It catches:
/// - A new `_namespace_fn` added without its `namespace.fn` alias.
/// - A function removed from the implementation but left in the registry.
/// - A signature mismatch between the registry and the implementation.
#[test]
fn bridge_covers_registry() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let registry_path =
        manifest_dir.join("../foundation/platform-architecture/function-registry.toml");

    let toml_str = std::fs::read_to_string(&registry_path).unwrap_or_else(|e| {
        panic!(
            "bridge_covers_registry: Cannot read function-registry.toml at {:?}: {}",
            registry_path, e
        )
    });

    let registry: Registry =
        toml::from_str(&toml_str).expect("bridge_covers_registry: Failed to parse function-registry.toml");

    let engine = Engine::default();
    let linker = create_linker(&engine)
        .expect("bridge_covers_registry: Failed to create server linker");

    let mut missing: Vec<String> = Vec::new();

    for func in registry.functions.iter().filter(|f| {
        (f.layer == 2 || f.layer == 3)
            && (f.hosts.is_empty() || f.hosts.iter().any(|h| h == "server"))
    }) {
        // Probe canonical name.
        let wat = single_import_wat(&func.module, &func.name, &func.params, &func.returns);
        if let Ok(module) = Module::new(&engine, &wat) {
            let mut store = make_store(&engine);
            if linker.instantiate(&mut store, &module).is_err() {
                missing.push(format!("canonical  {}::{}", func.module, func.name));
            }
        }

        // Probe every alias.
        for alias in &func.aliases {
            let wat = single_import_wat(&func.module, alias, &func.params, &func.returns);
            if let Ok(module) = Module::new(&engine, &wat) {
                let mut store = make_store(&engine);
                if linker.instantiate(&mut store, &module).is_err() {
                    missing.push(format!(
                        "alias      {}::{} (of {})",
                        func.module, alias, func.name
                    ));
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "bridge_covers_registry FAILED — {} registration(s) missing from the server linker:\n\n{}\n\n\
         Add each missing function/alias to the appropriate registration function.\n\
         Use the register_bridge_fn! macro for new _namespace_fn functions.\n\
         Never modify function-registry.toml to make this test pass.",
        missing.len(),
        missing.join("\n")
    );

    eprintln!(
        "bridge_covers_registry PASSED — all registry entries resolved by the server linker."
    );
}
