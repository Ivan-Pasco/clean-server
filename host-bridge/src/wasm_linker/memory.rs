//! Memory Runtime Host Functions
//!
//! Provides memory management functions for WASM modules:
//! - mem_alloc: Allocate memory in WASM linear memory
//! - mem_retain: Mark memory as retained (no-op for bump allocator)
//! - mem_release: Release memory (no-op for bump allocator)
//! - mem_scope_push/pop: Scope-based memory management (no-op for now)
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::state::WasmStateCore;
use crate::error::BridgeResult;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Register all memory runtime functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // MEMORY ALLOCATION
    // =========================================

    // mem_alloc - Allocate memory in WASM linear memory
    linker.func_wrap(
        "memory_runtime",
        "mem_alloc",
        |mut caller: Caller<'_, S>, size: i32, _align: i32| -> i32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => {
                    error!("mem_alloc: No memory export found");
                    return 0;
                }
            };

            let size = size as usize;

            // Get allocation offset from state (using trait method)
            let state = caller.data_mut();
            let ptr = state.memory_mut().allocate(size);

            // Ensure memory is large enough
            let required = ptr + size;
            let current_size = memory.data_size(&caller);

            debug!("mem_alloc: size={}, ptr={}, required={}, current_size={}", size, ptr, required, current_size);

            if required > current_size {
                // Calculate required pages (64KB per page)
                let required_pages = ((required + 65535) / 65536) as u64;
                let current_pages = memory.size(&caller);
                let pages_to_grow = required_pages.saturating_sub(current_pages);

                debug!("mem_alloc: growing from {} to {} pages", current_pages, required_pages);

                if pages_to_grow > 0 {
                    match memory.grow(&mut caller, pages_to_grow) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("mem_alloc: Failed to grow memory: {}", e);
                            return 0;
                        }
                    }
                }
            }

            ptr as i32
        },
    )?;

    // =========================================
    // MEMORY RETAIN/RELEASE (reference counting stubs)
    // =========================================

    // mem_retain - Increment reference count (no-op for bump allocator)
    linker.func_wrap(
        "memory_runtime",
        "mem_retain",
        |_: Caller<'_, S>, _ptr: i32| {
            // No-op: bump allocator doesn't track references
        },
    )?;

    // mem_release - Decrement reference count (no-op for bump allocator)
    linker.func_wrap(
        "memory_runtime",
        "mem_release",
        |_: Caller<'_, S>, _ptr: i32| {
            // No-op: bump allocator doesn't free individual allocations
        },
    )?;

    // =========================================
    // SCOPE-BASED MEMORY MANAGEMENT
    // =========================================

    // mem_scope_push - Push a new memory scope
    linker.func_wrap(
        "memory_runtime",
        "mem_scope_push",
        |_: Caller<'_, S>| {
            // No-op for now: could be used for scope-based cleanup
        },
    )?;

    // mem_scope_pop - Pop a memory scope (and free all allocations in it)
    linker.func_wrap(
        "memory_runtime",
        "mem_scope_pop",
        |_: Caller<'_, S>| {
            // No-op for now: could reset allocator to scope start
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Memory allocation tests require full WASM runtime
}
