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

// Re-export core types
pub use state::{WasmState, WasmStateCore, WasmMemory, RequestContext, AuthContext, SharedDbBridge};
pub use helpers::{
    read_string_from_caller, write_string_to_caller,
    read_raw_string, write_bytes_to_caller,
    read_length_prefixed_bytes, allocate_at_memory_end,
    read_raw_bytes, STRING_LENGTH_PREFIX_SIZE
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

    #[test]
    fn test_create_linker() {
        let engine = Engine::default();
        let linker = create_linker(&engine);
        assert!(linker.is_ok());
    }
}
