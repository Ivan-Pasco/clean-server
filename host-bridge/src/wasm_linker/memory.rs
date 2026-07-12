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

    // mem_alloc - Allocate memory in WASM linear memory.
    //
    // Compiler-emitted calling convention is `mem_alloc(type_id: i32, size: i32) -> i32`
    // (verified against the compiler's own wasmtime_runner — see clean-language-compiler/
    // src/bin/wasmtime_runner.rs). The first argument is a type tag that the compiler
    // uses for telemetry; the host implementation ignores it. The second argument is
    // the byte size to allocate.
    //
    // Earlier revisions of this function treated the args as `(size, align)`, which
    // made every allocation 0 bytes — every box returned the same pointer and
    // overwrote the previous one. Symptom: json.encode of any object literal trapped
    // inside the tagged-tree walker (see RUNTIME-WASMTIME-DIVERGES-FROM-CLN-TEST).
    //
    // CRITICAL: The host bump allocator (state.memory) and the WASM module's own
    // bump allocator (__malloc / list.allocate) share the same linear memory and the
    // same high-water mark (`__heap_ptr` global). They must be kept in sync on every
    // host allocation, otherwise the WASM-internal __malloc and the host mem_alloc
    // hand out overlapping ranges. Mirrors the read-then-take-max-then-write-back
    // pattern used by the compiler's wasmtime_runner.
    linker.func_wrap(
        "memory_runtime",
        "mem_alloc",
        |mut caller: Caller<'_, S>, _type_id: i32, size: i32| -> i32 {
            // Step 1: Sync host offset with WASM __heap_ptr before allocating.
            if let Some(heap_global) = caller
                .get_export("__heap_ptr")
                .and_then(|e| e.into_global())
            {
                if let Some(wasm_heap) = heap_global.get(&mut caller).i32() {
                    let wasm_heap = wasm_heap as usize;
                    let host_off = caller.data().memory().current_offset();
                    if wasm_heap > host_off {
                        caller.data_mut().memory_mut().set_offset(wasm_heap);
                    }
                }
            }

            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => {
                    error!("mem_alloc: No memory export found");
                    return 0;
                }
            };

            let size = size as usize;

            // Get allocation offset from state (using trait method).
            let state = caller.data_mut();
            let ptr = state.memory_mut().allocate(size);

            // Ensure memory is large enough using 1.5x amortized growth.
            let required = ptr + size;
            let current_size = memory.data_size(&caller);

            debug!(
                "mem_alloc: size={}, ptr={}, required={}, current_size={}",
                size, ptr, required, current_size
            );

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
                                "mem_alloc: Failed to grow memory: {}",
                                e
                            );
                            return 0;
                        }
                    }
                }
            }

            // Step 2: Write the new high-water mark back to __heap_ptr so the
            // WASM-side __malloc resumes from past our allocation.
            let new_offset = caller.data().memory().current_offset() as i32;
            if let Some(heap_global) = caller
                .get_export("__heap_ptr")
                .and_then(|e| e.into_global())
            {
                let current = heap_global.get(&mut caller).i32().unwrap_or(0);
                if new_offset > current {
                    let _ = heap_global.set(&mut caller, wasmtime::Val::I32(new_offset));
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
    linker.func_wrap("memory_runtime", "mem_scope_push", |_: Caller<'_, S>| {
        // No-op for now: could be used for scope-based cleanup
    })?;

    // mem_scope_pop - Pop a memory scope (and free all allocations in it)
    linker.func_wrap("memory_runtime", "mem_scope_pop", |_: Caller<'_, S>| {
        // No-op for now: could reset allocator to scope start
    })?;

    // =========================================
    // ARENA SCOPE (compiler 0.30.391+)
    // =========================================
    //
    // The compiler's dual-accumulator HIR rewrite (rewrite_dual_accumulator_loops)
    // brackets per-iteration scratch allocations with `_arena_scope_push` /
    // `_arena_scope_pop` so the bump-arena offset can be rewound at the end of
    // each iteration. Without these imports framework plugins built with
    // cln >= 0.30.391 fail to instantiate with "unknown import".

    // _arena_scope_push - snapshot the bump-arena offset and return the new
    // mark-stack depth as an opaque handle (always >= 1).
    linker.func_wrap(
        "env",
        "_arena_scope_push",
        |mut caller: Caller<'_, S>| -> i32 {
            let depth = caller.data_mut().memory_mut().push_arena_mark();
            depth as i32
        },
    )?;

    linker.alias("env", "_arena_scope_push", "env", "arena.scope_push")?;

    // _arena_scope_pop - rewind the bump-arena offset to the mark saved by the
    // matching push, reclaiming all allocations made in between. `handle <= 0`
    // is treated as a no-op so generated code that early-returns past the push
    // does not trap.
    linker.func_wrap(
        "env",
        "_arena_scope_pop",
        |mut caller: Caller<'_, S>, handle: i32| {
            if handle <= 0 {
                return;
            }
            let target_depth = (handle as usize).saturating_sub(1);
            caller.data_mut().memory_mut().pop_arena_mark(target_depth);
        },
    )?;

    linker.alias("env", "_arena_scope_pop", "env", "arena.scope_pop")?;

    // =========================================
    // STATE RESET (compiler 0.30.155+)
    // =========================================

    // _state_reset_all - Reset all WASM frame state between handler invocations.
    // Generated by compiler 0.30.155+ in every compiled module.
    // Resets the bump allocator so each request starts with a clean allocation slate.
    linker.func_wrap("env", "_state_reset_all", |mut caller: Caller<'_, S>| {
        caller.data_mut().memory_mut().reset();
    })?;

    linker.alias("env", "_state_reset_all", "env", "state.reset_all")?;

    // _state_reset_named - Reset a named state variable
    // Signature: (name_ptr: i32) -> void
    // No-op for bump allocator; name_ptr points to the variable name (unused here).
    linker.func_wrap(
        "env",
        "_state_reset_named",
        |_: Caller<'_, S>, _name_ptr: i32| {
            // No-op: named state reset not applicable to bump allocator
        },
    )?;

    linker.alias("env", "_state_reset_named", "env", "state.reset_named")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Memory allocation tests require full WASM runtime
}
