//! List Host Functions
//!
//! Provides list operations that require host-side memory coordination.
//! Most list functions are compiled into the WASM module itself, but
//! `list.push_f64` requires host involvement for memory management.
//!
//! ## List Memory Layout (16-byte header)
//!
//! ```text
//! [0..4]   size     (u32 LE) - current number of elements
//! [4..8]   capacity (u32 LE) - allocated capacity
//! [8..12]  type_id  (u32 LE) - list type identifier
//! [12..16] padding  (u32 LE) - reserved
//! [16+]    elements           - f64 values at 8 bytes each
//! ```

use super::state::WasmStateCore;
use crate::error::BridgeResult;
use std::cell::RefCell;
use std::collections::HashMap;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

const LIST_HEADER_SIZE: usize = 16;
const F64_SIZE: usize = 8;

// Handle-based list store. Distinct from the memory-backed list<T> used by
// list.push_f64.
thread_local! {
    static LIST_STORE: RefCell<HashMap<i32, Vec<i32>>> = RefCell::new(HashMap::new());
    static NEXT_LIST_HANDLE: RefCell<i32> = const { RefCell::new(1) };
}

/// Reset the thread-local list store. Call between requests.
pub fn reset_list_store() {
    LIST_STORE.with(|s| s.borrow_mut().clear());
    NEXT_LIST_HANDLE.with(|n| *n.borrow_mut() = 1);
}

fn new_list_handle(initial: Vec<i32>) -> i32 {
    let handle = NEXT_LIST_HANDLE.with(|n| {
        let mut v = n.borrow_mut();
        let h = *v;
        *v = v.checked_add(1).unwrap_or(1);
        h
    });
    LIST_STORE.with(|s| s.borrow_mut().insert(handle, initial));
    handle
}

/// Register all list functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // list.push_f64 - Push an f64 value onto a list (in-place mutation)
    // Signature: (list_ptr: i32, value: f64) -> i32
    linker.func_wrap(
        "env",
        "list.push_f64",
        |mut caller: Caller<'_, S>, list_ptr: i32, value: f64| -> i32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => {
                    error!("list.push_f64: No memory export found");
                    return 0;
                }
            };

            let ptr = list_ptr as usize;
            let data = memory.data(&caller);

            // Bounds check for header
            if ptr + LIST_HEADER_SIZE > data.len() {
                error!("list.push_f64: list header out of bounds at ptr={}", ptr);
                return 0;
            }

            // Read current length
            let length = u32::from_le_bytes(data[ptr..ptr + 4].try_into().unwrap()) as usize;

            // Read capacity
            let capacity = u32::from_le_bytes(data[ptr + 4..ptr + 8].try_into().unwrap()) as usize;

            debug!(
                "list.push_f64: ptr={}, length={}, capacity={}, value={}",
                ptr, length, capacity, value
            );

            // Check there's room (capacity must be > length)
            if length >= capacity {
                error!(
                    "list.push_f64: list is full (length={}, capacity={})",
                    length, capacity
                );
                return list_ptr;
            }

            // Calculate element offset
            let element_offset = ptr + LIST_HEADER_SIZE + length * F64_SIZE;

            // Bounds check for the new element
            if element_offset + F64_SIZE > data.len() {
                error!(
                    "list.push_f64: element write out of bounds at offset={}",
                    element_offset
                );
                return list_ptr;
            }

            // Write the f64 value
            let value_bytes = value.to_le_bytes();
            let data_mut = memory.data_mut(&mut caller);
            data_mut[element_offset..element_offset + F64_SIZE].copy_from_slice(&value_bytes);

            // Increment length
            let new_length = (length + 1) as u32;
            data_mut[ptr..ptr + 4].copy_from_slice(&new_length.to_le_bytes());

            debug!(
                "list.push_f64: pushed value={} at index={}, new_length={}",
                value, length, new_length
            );
            list_ptr
        },
    )?;

    // =========================================
    // Handle-based list ops.
    // These take an i32 handle, NOT a WASM memory pointer.
    // =========================================

    // list.allocate(capacity_hint) -> handle
    linker.func_wrap(
        "env",
        "list.allocate",
        |_: Caller<'_, S>, capacity: i32| -> i32 {
            let cap = capacity.max(0) as usize;
            new_list_handle(Vec::with_capacity(cap))
        },
    )?;

    // list.add(handle, value) -> void  (alias of push)
    linker.func_wrap(
        "env",
        "list.add",
        |_: Caller<'_, S>, handle: i32, value: i32| {
            LIST_STORE.with(|s| {
                if let Some(v) = s.borrow_mut().get_mut(&handle) {
                    v.push(value);
                }
            });
        },
    )?;

    // list.push(handle, value) -> void
    linker.func_wrap(
        "env",
        "list.push",
        |_: Caller<'_, S>, handle: i32, value: i32| {
            LIST_STORE.with(|s| {
                if let Some(v) = s.borrow_mut().get_mut(&handle) {
                    v.push(value);
                }
            });
        },
    )?;

    // list.clear(handle) -> void
    linker.func_wrap("env", "list.clear", |_: Caller<'_, S>, handle: i32| {
        LIST_STORE.with(|s| {
            if let Some(v) = s.borrow_mut().get_mut(&handle) {
                v.clear();
            }
        });
    })?;

    // list.contains(handle, value) -> boolean
    linker.func_wrap(
        "env",
        "list.contains",
        |_: Caller<'_, S>, handle: i32, value: i32| -> i32 {
            LIST_STORE.with(|s| match s.borrow().get(&handle) {
                Some(v) if v.contains(&value) => 1,
                _ => 0,
            })
        },
    )?;

    // list.get(handle, index) -> i32 (0 on OOB/invalid)
    linker.func_wrap(
        "env",
        "list.get",
        |_: Caller<'_, S>, handle: i32, idx: i32| -> i32 {
            if idx < 0 {
                return 0;
            }
            LIST_STORE.with(|s| {
                s.borrow()
                    .get(&handle)
                    .and_then(|v| v.get(idx as usize).copied())
                    .unwrap_or(0)
            })
        },
    )?;

    // list.set(handle, index, value) -> void
    linker.func_wrap(
        "env",
        "list.set",
        |_: Caller<'_, S>, handle: i32, idx: i32, value: i32| {
            if idx < 0 {
                return;
            }
            LIST_STORE.with(|s| {
                if let Some(v) = s.borrow_mut().get_mut(&handle) {
                    if (idx as usize) < v.len() {
                        v[idx as usize] = value;
                    }
                }
            });
        },
    )?;

    // list.remove(handle, index) -> void
    linker.func_wrap(
        "env",
        "list.remove",
        |_: Caller<'_, S>, handle: i32, idx: i32| {
            if idx < 0 {
                return;
            }
            LIST_STORE.with(|s| {
                if let Some(v) = s.borrow_mut().get_mut(&handle) {
                    if (idx as usize) < v.len() {
                        v.remove(idx as usize);
                    }
                }
            });
        },
    )?;

    // list.isEmpty(handle) -> boolean
    linker.func_wrap(
        "env",
        "list.isEmpty",
        |_: Caller<'_, S>, handle: i32| -> i32 {
            LIST_STORE.with(|s| {
                match s.borrow().get(&handle) {
                    Some(v) if v.is_empty() => 1,
                    Some(_) => 0,
                    None => 1, // invalid handle treated as empty
                }
            })
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests would require WASM runtime setup
}
