//! Full Bridge Compliance Test
//!
//! Validates that the clean-server linker can instantiate a WASM module that
//! imports every host function defined across both Layer 2 (host-bridge) and
//! Layer 3 (server-specific) of the platform function registry.
//!
//! This test is the authoritative single-module proof that the complete server
//! bridge satisfies every contract in `foundation/spec/platform/function-registry.toml`.
//!
//! # How It Works
//!
//! 1. Reads `function-registry.toml` at runtime (same relative-path approach
//!    used by `test_spec_compliance` in host-bridge and `test_layer3_spec_compliance`
//!    in bridge.rs).
//! 2. Generates a WAT module whose only imports are all functions from all
//!    layers (canonical names AND aliases).
//! 3. Creates the full server linker via `clean_server::bridge::create_linker`
//!    which registers both Layer 2 (host-bridge) and Layer 3 (bridge.rs)
//!    functions.
//! 4. Instantiates the WAT module against the linker and asserts success.
//!
//! If any function is missing or has an incorrect signature, wasmtime will
//! name the exact failing import in the error message.
//!
//! # Type Expansion Rules
//!
//! The registry uses high-level types that expand to WASM primitive types:
//!
//! | Registry type | WASM param(s)  | WASM return |
//! |---------------|----------------|-------------|
//! | `"string"`    | `(i32 i32)`    | —           |
//! | `"integer"`   | `(i64)`        | `i64`       |
//! | `"number"`    | `(f64)`        | `f64`       |
//! | `"boolean"`   | `(i32)`        | `i32`       |
//! | `"i32"`       | `(i32)`        | `i32`       |
//! | `"i64"`       | `(i64)`        | `i64`       |
//! | `"ptr"`       | —              | `i32`       |
//! | `"void"`      | —              | —           |

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::path::PathBuf;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

// ---------------------------------------------------------------------------
// Registry TOML data model
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
    // See registry header "HOST SCOPING". Server-side compliance only
    // exercises entries whose `hosts` includes "server" (or is empty).
    #[serde(default)]
    hosts: Vec<String>,
}

// ---------------------------------------------------------------------------
// Type expansion helpers
// ---------------------------------------------------------------------------

/// Type-expansion convention selector. See bridge_contract_test.rs for the
/// full rationale — the short version: the registry uses "integer" for two
/// different WASM types depending on the function's ownership.
///
/// - `HostBridge` — pure Layer 2/3 host functions (math, db, http_client
///   internals). `integer` → `i64` (native register width).
/// - `PluginBridge` — plugin-side functions with `hosts` containing
///   "browser" (frame.ui, frame.canvas). `integer` → `i32` because the
///   compiler emits `i32` on the plugin-bridge ABI so the browser JS
///   shim can consume it directly. Server no-op stubs must match that.
#[derive(Copy, Clone)]
enum TypeConvention {
    HostBridge,
    PluginBridge,
}

fn convention_for(hosts: &[String]) -> TypeConvention {
    if hosts.iter().any(|h| h == "browser") {
        TypeConvention::PluginBridge
    } else {
        TypeConvention::HostBridge
    }
}

fn expand_param_type(t: &str, conv: TypeConvention) -> Vec<&str> {
    match t {
        "string" => vec!["i32", "i32"],
        "integer" => match conv {
            TypeConvention::HostBridge => vec!["i64"],
            TypeConvention::PluginBridge => vec!["i32"],
        },
        "number" => vec!["f64"],
        "boolean" => vec!["i32"],
        "i32" => vec!["i32"],
        "i64" => vec!["i64"],
        // "any" is a Clean Language boxed-any pointer (i32 heap address).
        "any" => vec!["i32"],
        // "ptr" param is a single i32 pointing into linear memory — the
        // callee reads a length prefix (or other framing) from that address.
        // Mirrors the return-type expansion above.
        "ptr" => vec!["i32"],
        other => panic!(
            "Unknown parameter type in function-registry.toml: '{}'. \
             Update expand_param_type() in bridge_compliance_test.rs if a new type was added.",
            other
        ),
    }
}

fn expand_return_type(t: &str, conv: TypeConvention) -> Option<&str> {
    match t {
        "void" => None,
        "ptr" => Some("i32"),
        "string" => Some("i32"), // string return = ptr to length-prefixed string
        "i32" => Some("i32"),
        "i64" => Some("i64"),
        "boolean" => Some("i32"),
        "integer" => match conv {
            TypeConvention::HostBridge => Some("i64"),
            TypeConvention::PluginBridge => Some("i32"),
        },
        "number" => Some("f64"),
        // "any" is a Clean Language boxed-any pointer (i32 heap address).
        "any" => Some("i32"),
        other => panic!(
            "Unknown return type in function-registry.toml: '{}'. \
             Update expand_return_type() in bridge_compliance_test.rs if a new type was added.",
            other
        ),
    }
}

/// Generate a single WAT import declaration line for the given function.
fn generate_wat_import(
    module: &str,
    name: &str,
    params: &[String],
    returns: &str,
    conv: TypeConvention,
) -> String {
    let mut import = format!("  (import \"{}\" \"{}\" (func", module, name);

    let wasm_params: Vec<&str> = params
        .iter()
        .flat_map(|t| expand_param_type(t, conv))
        .collect();

    if !wasm_params.is_empty() {
        import.push_str(" (param");
        for p in &wasm_params {
            import.push_str(&format!(" {}", p));
        }
        import.push(')');
    }

    if let Some(ret) = expand_return_type(returns, conv) {
        import.push_str(&format!(" (result {})", ret));
    }

    import.push_str("))\n");
    import
}

// ---------------------------------------------------------------------------
// Store helper
// ---------------------------------------------------------------------------

fn make_store(engine: &Engine) -> Store<WasmState> {
    let router = Arc::new(Router::new());
    let state = WasmState::new(router);
    Store::new(engine, state)
}

// ---------------------------------------------------------------------------
// The compliance test
// ---------------------------------------------------------------------------

/// Verifies that the clean-server linker satisfies every import defined in
/// `foundation/spec/platform/function-registry.toml` for both Layer 2 and Layer 3.
///
/// The test generates a WAT module with one import per registered function
/// (canonical name plus all aliases) and instantiates it against the full
/// server linker produced by `create_linker`.  If any function is absent or
/// carries the wrong WASM signature, wasmtime reports the exact failing import.
///
/// Passing this test means:
/// - No registered function is accidentally missing from the linker.
/// - No registered function has a signature mismatch between the registry and
///   the implementation.
/// - Both Layer 2 (portable host-bridge) and Layer 3 (server extensions) are
///   validated in a single instantiation attempt.
#[test]
fn test_full_bridge_compliance() {
    // Locate the registry file relative to this crate's manifest directory.
    // clean-server is at:  <project-root>/clean-server/
    // registry is at:      <project-root>/foundation/spec/platform/function-registry.toml
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let registry_path =
        manifest_dir.join("../foundation/spec/platform/function-registry.toml");

    let toml_str = std::fs::read_to_string(&registry_path).unwrap_or_else(|e| {
        panic!(
            "test_full_bridge_compliance: Cannot read function-registry.toml at {:?}.\n\
             Ensure the foundation/spec/platform directory exists at the project root.\n\
             Error: {}",
            registry_path
                .canonicalize()
                .unwrap_or(registry_path.clone()),
            e
        )
    });

    let registry: Registry = toml::from_str(&toml_str).unwrap_or_else(|e| {
        panic!(
            "test_full_bridge_compliance: Failed to parse function-registry.toml: {}",
            e
        )
    });

    // Collect all functions from every layer (Layer 2 + Layer 3) whose
    // `hosts` field includes "server" or is empty (unrestricted). Browser-
    // only functions (frame.ui client-side stubs, canvas draw primitives)
    // are intentionally NOT registered on the server and must be excluded
    // here — otherwise the module fails to instantiate with a linker error.
    // Matches the filter in bridge_contract_test.rs.
    let all_funcs: Vec<&FunctionEntry> = registry
        .functions
        .iter()
        .filter(|f| {
            (f.layer == 2 || f.layer == 3)
                && (f.hosts.is_empty() || f.hosts.iter().any(|h| h == "server"))
        })
        .collect();

    // Sanity-check that the registry was loaded with a reasonable number of entries.
    // As of the registry version that includes all current functions, there are
    // at least 110 canonical entries across both layers.
    assert!(
        all_funcs.len() >= 110,
        "test_full_bridge_compliance: Expected at least 110 canonical functions across \
         Layer 2 and Layer 3 in the registry, found {}. \
         Verify that the registry file is complete.",
        all_funcs.len()
    );

    // Count per layer for the summary output.
    let layer2_canonical: usize = all_funcs.iter().filter(|f| f.layer == 2).count();
    let layer3_canonical: usize = all_funcs.iter().filter(|f| f.layer == 3).count();

    // Build the WAT module header.  All imports must precede any local
    // definitions in the WASM text format.
    let mut wat = String::from("(module\n");
    wat.push_str("  ;; Full bridge compliance: Layer 2 (host-bridge) + Layer 3 (server)\n");
    wat.push_str("  ;; Generated from foundation/spec/platform/function-registry.toml\n\n");

    let mut import_count: usize = 0;
    let mut layer2_import_total: usize = 0;
    let mut layer3_import_total: usize = 0;

    // Emit Layer 2 imports first (memory_runtime module comes before env to
    // keep the order predictable and match the existing WAT contract).
    wat.push_str("  ;; --- Layer 2: memory_runtime module ---\n");
    for func in all_funcs
        .iter()
        .filter(|f| f.layer == 2 && f.module == "memory_runtime")
    {
        let conv = convention_for(&func.hosts);
        wat.push_str(&generate_wat_import(
            &func.module,
            &func.name,
            &func.params,
            &func.returns,
            conv,
        ));
        import_count += 1;
        layer2_import_total += 1;
        for alias in &func.aliases {
            wat.push_str(&generate_wat_import(
                &func.module,
                alias,
                &func.params,
                &func.returns,
                conv,
            ));
            import_count += 1;
            layer2_import_total += 1;
        }
    }

    wat.push_str("\n  ;; --- Layer 2: env module ---\n");
    for func in all_funcs
        .iter()
        .filter(|f| f.layer == 2 && f.module == "env")
    {
        let conv = convention_for(&func.hosts);
        wat.push_str(&generate_wat_import(
            &func.module,
            &func.name,
            &func.params,
            &func.returns,
            conv,
        ));
        import_count += 1;
        layer2_import_total += 1;
        for alias in &func.aliases {
            wat.push_str(&generate_wat_import(
                &func.module,
                alias,
                &func.params,
                &func.returns,
                conv,
            ));
            import_count += 1;
            layer2_import_total += 1;
        }
    }

    wat.push_str("\n  ;; --- Layer 3: env module (server-specific) ---\n");
    for func in all_funcs.iter().filter(|f| f.layer == 3) {
        let conv = convention_for(&func.hosts);
        wat.push_str(&generate_wat_import(
            &func.module,
            &func.name,
            &func.params,
            &func.returns,
            conv,
        ));
        import_count += 1;
        layer3_import_total += 1;
        for alias in &func.aliases {
            wat.push_str(&generate_wat_import(
                &func.module,
                alias,
                &func.params,
                &func.returns,
                conv,
            ));
            import_count += 1;
            layer3_import_total += 1;
        }
    }

    // Close the WAT module.  No local definitions are needed because the
    // test only validates that all imports are satisfiable by the linker.
    wat.push_str(")\n");

    eprintln!(
        "test_full_bridge_compliance: Generated WAT with {} total imports \
         ({} Layer 2 including aliases, {} Layer 3 including aliases)",
        import_count, layer2_import_total, layer3_import_total,
    );

    // Create the engine and parse the generated WAT into a WASM module.
    let engine = Engine::default();

    let module = Module::new(&engine, &wat).unwrap_or_else(|e| {
        panic!(
            "test_full_bridge_compliance: Generated WAT failed to parse into a WASM module.\n\
             This is a bug in the test's WAT generation logic.\n\
             Error: {}\n\n\
             --- Generated WAT ({} imports) ---\n{}",
            e, import_count, wat
        )
    });

    // Create the full server linker (Layer 2 from host-bridge + Layer 3 from
    // bridge.rs) and attempt instantiation.
    let linker = create_linker(&engine).unwrap_or_else(|e| {
        panic!(
            "test_full_bridge_compliance: Failed to create the server linker: {}",
            e
        )
    });

    let mut store = make_store(&engine);

    linker.instantiate(&mut store, &module).unwrap_or_else(|e| {
        panic!(
            "test_full_bridge_compliance: FULL BRIDGE COMPLIANCE FAILURE\n\n\
             The server linker cannot satisfy all {} imports defined in \
             function-registry.toml.\n\n\
             Wasmtime error (identifies the exact failing import):\n  {}\n\n\
             Resolution steps:\n\
             1. The error message above names the exact missing or mismatched function.\n\
             2. If the function is missing: add it to the appropriate registration \
                function in src/bridge.rs (Layer 3) or \
                host-bridge/src/wasm_linker/ (Layer 2).\n\
             3. If the signature is wrong: fix the implementation to match the \
                registry type definitions in function-registry.toml.\n\
             4. Never modify the registry to make this test pass — fix the \
                implementation instead.",
            import_count, e
        )
    });

    // Report a detailed summary so test output is informative on success.
    let actual_import_count = module.imports().count();
    eprintln!(
        "test_full_bridge_compliance: PASSED\n  \
         Registry canonical functions: {} Layer 2, {} Layer 3 ({} total)\n  \
         WAT imports generated (canonical + aliases): {} L2, {} L3, {} combined\n  \
         WASM module import count: {}\n  \
         All imports satisfied by the full server linker.",
        layer2_canonical,
        layer3_canonical,
        layer2_canonical + layer3_canonical,
        layer2_import_total,
        layer3_import_total,
        import_count,
        actual_import_count,
    );
}
