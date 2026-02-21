//! WASM Linker Host Functions
//!
//! Provides all host functions required by Clean Language WASM modules.
//! This module creates wasmtime-compatible host function bindings.
//!
//! ## Generic Architecture
//!
//! All registration functions are generic over `WasmStateCore`, allowing any
//! runtime (CLI, server, embedded) to use them with their own state type.
//!
//! ## Host Function Categories
//!
//! ### Platform I/O (requires host access)
//! - Console: print, printl, input, input_*
//! - File: file_read, file_write, file_exists, file_delete, file_append
//! - HTTP Client: http_get, http_post, etc.
//! - Database: _db_query, _db_execute, etc.
//!
//! ### Advanced Math (no native WASM instructions)
//! - Trigonometric: sin, cos, tan, asin, acos, atan, atan2
//! - Hyperbolic: sinh, cosh, tanh
//! - Logarithmic: ln, log10, log2, exp, exp2, pow, sqrt
//!
//! ### String Allocation (requires memory management)
//! - concat, substring, trim, toUpper, toLower, replace, split
//!
//! ### Memory Runtime
//! - mem_alloc, mem_retain, mem_release, mem_scope_push, mem_scope_pop

mod state;
mod console;
mod math;
mod string_ops;
mod memory;
mod helpers;
mod database;
mod file_io;
mod http_client;
mod crypto_funcs;
mod env_time;
// NOTE: HTTP Server functions (Layer 3) are NOT in host-bridge.
// They are server-specific and implemented in clean-server/src/bridge.rs.
// See platform-architecture/EXECUTION_LAYERS.md for layer definitions.

// Re-export core types
pub use state::{WasmState, WasmStateCore, WasmMemory, RequestContext, AuthContext, SharedDbBridge, HttpResponseBuilder};
pub use helpers::{
    read_string_from_caller, write_string_to_caller,
    read_raw_string, write_bytes_to_caller,
    read_length_prefixed_bytes, read_raw_bytes, STRING_LENGTH_PREFIX_SIZE
};

use crate::error::BridgeResult;
use wasmtime::{Engine, Linker};

/// Register all bridge host functions with a linker
///
/// This is the main entry point for any runtime. It registers:
/// - Console I/O (print, input)
/// - Math functions (sin, cos, pow, etc.)
/// - String operations (concat, substring, etc.)
/// - Memory runtime (mem_alloc, etc.)
/// - Database operations (_db_query, _db_execute, etc.)
/// - File I/O (file_read, file_write, etc.)
/// - HTTP client (http_get, http_post, etc.)
/// - Crypto (password hashing, etc.)
///
/// # Type Parameter
///
/// `S` must implement `WasmStateCore` which provides access to the memory allocator.
///
/// # Example
///
/// ```ignore
/// use host_bridge::wasm_linker::{register_all_functions, WasmState};
/// use wasmtime::{Engine, Linker};
///
/// let engine = Engine::default();
/// let mut linker = Linker::new(&engine);
/// register_all_functions(&mut linker)?;
/// ```
pub fn register_all_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // Core functions
    console::register_functions(linker)?;
    math::register_functions(linker)?;
    string_ops::register_functions(linker)?;
    memory::register_functions(linker)?;

    // Platform I/O
    database::register_functions(linker)?;
    file_io::register_functions(linker)?;
    http_client::register_functions(linker)?;
    crypto_funcs::register_functions(linker)?;
    env_time::register_functions(linker)?;

    // NOTE: HTTP Server functions (Layer 3) are NOT provided by host-bridge.
    // Server-specific functions like _req_param, _req_body, _http_route, etc.
    // must be implemented by the server runtime (e.g., clean-server/src/bridge.rs).
    // See platform-architecture/EXECUTION_LAYERS.md for layer definitions.

    Ok(())
}

/// Create a fully configured linker with all host functions
///
/// Convenience function that creates a new linker and registers all functions.
/// Uses the default `WasmState` type from host-bridge.
pub fn create_linker(engine: &Engine) -> BridgeResult<Linker<WasmState>> {
    let mut linker = Linker::new(engine);
    register_all_functions(&mut linker)?;
    Ok(linker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Module, Store};

    #[test]
    fn test_create_linker() {
        let engine = Engine::default();
        let linker = create_linker(&engine);
        assert!(linker.is_ok());
    }

    // --- Registry TOML types ---

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
    }

    fn expand_param_type(t: &str) -> Vec<&str> {
        match t {
            "string" => vec!["i32", "i32"],
            "integer" => vec!["i64"],
            "number" => vec!["f64"],
            "boolean" => vec!["i32"],
            "i32" => vec!["i32"],
            "i64" => vec!["i64"],
            other => panic!("Unknown param type in registry: '{}'", other),
        }
    }

    fn expand_return_type(t: &str) -> Option<&str> {
        match t {
            "void" => None,
            "ptr" => Some("i32"),
            "i32" => Some("i32"),
            "i64" => Some("i64"),
            "boolean" => Some("i32"),
            "integer" => Some("i64"),
            "number" => Some("f64"),
            other => panic!("Unknown return type in registry: '{}'", other),
        }
    }

    fn generate_wat_import(module: &str, name: &str, params: &[String], returns: &str) -> String {
        let mut import = format!("  (import \"{}\" \"{}\" (func", module, name);

        let wasm_params: Vec<&str> = params.iter()
            .flat_map(|t| expand_param_type(t))
            .collect();

        if !wasm_params.is_empty() {
            import.push_str(" (param");
            for p in &wasm_params {
                import.push_str(&format!(" {}", p));
            }
            import.push(')');
        }

        if let Some(ret) = expand_return_type(returns) {
            import.push_str(&format!(" (result {})", ret));
        }

        import.push_str("))\n");
        import
    }

    /// Spec compliance test: validates that ALL Layer 2 host function signatures
    /// match the shared function registry (platform-architecture/function-registry.toml).
    ///
    /// The registry defines every function with high-level types that expand to
    /// exact WASM signatures. This test dynamically generates a WAT module from
    /// the registry and instantiates it against the linker. If any signature in
    /// the implementation differs from the registry, instantiation fails.
    ///
    /// To update: modify function-registry.toml first, then the implementation.
    /// Never change the registry just to pass tests.
    #[test]
    fn test_spec_compliance() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let registry_path = std::path::Path::new(manifest_dir)
            .join("../../platform-architecture/function-registry.toml");
        let toml_str = std::fs::read_to_string(&registry_path)
            .unwrap_or_else(|e| panic!(
                "Failed to read function-registry.toml at {:?}: {}",
                registry_path, e
            ));

        let registry: Registry = toml::from_str(&toml_str)
            .expect("Failed to parse function-registry.toml");

        // Filter for Layer 2 functions only (host-bridge scope)
        let layer2_funcs: Vec<&FunctionEntry> = registry.functions.iter()
            .filter(|f| f.layer == 2)
            .collect();

        assert!(
            layer2_funcs.len() >= 80,
            "Expected at least 80 Layer 2 canonical functions in registry, found {}",
            layer2_funcs.len()
        );

        // Generate WAT module with all Layer 2 imports
        let mut wat = String::from("(module\n");
        let mut import_count = 0;

        for func in &layer2_funcs {
            wat.push_str(&generate_wat_import(&func.module, &func.name, &func.params, &func.returns));
            import_count += 1;

            for alias in &func.aliases {
                wat.push_str(&generate_wat_import(&func.module, alias, &func.params, &func.returns));
                import_count += 1;
            }
        }

        wat.push_str(")\n");

        // Create linker and validate all signatures
        let engine = Engine::default();
        let linker = create_linker(&engine).expect("Failed to create linker");
        let module = Module::new(&engine, &wat)
            .unwrap_or_else(|e| panic!(
                "Failed to parse generated WAT ({} imports): {}\n\nGenerated WAT:\n{}",
                import_count, e, wat
            ));

        let mut store = Store::new(&engine, WasmState::default());

        linker.instantiate(&mut store, &module).unwrap_or_else(|e| panic!(
            "SPEC COMPLIANCE FAILURE ({} Layer 2 imports):\n{}\n\n\
             Fix the implementation to match function-registry.toml, not the other way around.",
            import_count, e
        ));

        eprintln!(
            "Layer 2 spec compliance PASSED: {} canonical + aliases = {} total imports",
            layer2_funcs.len(), import_count
        );
    }
}
