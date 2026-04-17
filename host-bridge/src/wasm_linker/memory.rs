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
use tracing::{debug, error, warn};
use wasmtime::{Caller, Linker};

/// WASM page size in bytes (64KB)
const PAGE_SIZE: usize = 65536;

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

            // Ensure memory is large enough using 1.5x amortized growth
            let required = ptr + size;
            let current_size = memory.data_size(&caller);

            debug!("mem_alloc: size={}, ptr={}, required={}, current_size={}", size, ptr, required, current_size);

            if required > current_size {
                let current_pages = memory.size(&caller);

                // 1.5x amortized growth: grow by at least 1.5x current, at least 4 pages floor
                let target = required
                    .max(current_size * 3 / 2)
                    .max(current_size + 4 * PAGE_SIZE);
                let target_pages = target.div_ceil(PAGE_SIZE) as u64;
                let pages_to_grow = target_pages.saturating_sub(current_pages);

                if pages_to_grow > 0 {
                    debug!(
                        event = "wasm_memory_grow",
                        current_pages = current_pages,
                        requested_pages = pages_to_grow,
                        new_total_pages = current_pages + pages_to_grow,
                        "mem_alloc: growing memory"
                    );

                    match memory.grow(&mut caller, pages_to_grow) {
                        Ok(_) => {
                            caller.data_mut().memory_mut().record_grow();
                        }
                        Err(e) => {
                            warn!(
                                event = "wasm_memory_oom",
                                current_pages = current_pages,
                                requested_pages = pages_to_grow,
                                "mem_alloc: Failed to grow memory: {}", e
                            );
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
