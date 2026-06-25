//! Array Host Functions — handle-based JS-style arrays
//!
//! Mirrors clean-node-server/src/bridge/array.ts. The `arr_ptr` argument
//! is an opaque i32 handle into a host-side store, NOT a WASM memory
//! pointer. Callback-taking variants (filter/map/reduce/find) invoke
//! WASM-side functions through the module's `__indirect_function_table`.
//!
//! These are independent of the Clean Language native `list<T>` ops
//! (which live in `list_funcs.rs` and operate on WASM-resident memory).

use super::state::WasmStateCore;
use crate::error::BridgeResult;
use std::cell::RefCell;
use std::collections::HashMap;
use tracing::{debug, warn};
use wasmtime::{Caller, Linker, Ref, Val};

// Per-thread store. Handles are scoped to the thread that ran the WASM
// instance; reset between requests via `reset_array_store`.
thread_local! {
    static ARRAY_STORE: RefCell<HashMap<i32, Vec<i32>>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<i32> = const { RefCell::new(1) };
}

/// Reset the thread-local array store. Call between requests so handles
/// from a previous WASM run don't leak into the next one.
pub fn reset_array_store() {
    ARRAY_STORE.with(|s| s.borrow_mut().clear());
    NEXT_HANDLE.with(|n| *n.borrow_mut() = 1);
}

fn store_array(arr: Vec<i32>) -> i32 {
    let handle = NEXT_HANDLE.with(|n| {
        let mut v = n.borrow_mut();
        let h = *v;
        *v = v.checked_add(1).unwrap_or(1);
        h
    });
    ARRAY_STORE.with(|s| s.borrow_mut().insert(handle, arr));
    handle
}

fn with_array<R>(handle: i32, f: impl FnOnce(&Vec<i32>) -> R) -> Option<R> {
    ARRAY_STORE.with(|s| s.borrow().get(&handle).map(f))
}

fn with_array_mut<R>(handle: i32, f: impl FnOnce(&mut Vec<i32>) -> R) -> Option<R> {
    ARRAY_STORE.with(|s| s.borrow_mut().get_mut(&handle).map(f))
}

/// Invoke a WASM callback through `__indirect_function_table[idx]` with the
/// given i32 arguments. Returns 0 if the table is missing, the callback is
/// uninitialized, or the call traps.
fn call_indirect<S: WasmStateCore>(
    caller: &mut Caller<'_, S>,
    callback_idx: i32,
    args: &[i32],
) -> i32 {
    let table = match caller
        .get_export("__indirect_function_table")
        .and_then(|e| e.into_table())
    {
        Some(t) => t,
        None => {
            warn!("array callback: no __indirect_function_table export");
            return 0;
        }
    };

    let func_ref = match table.get(&mut *caller, callback_idx as u64) {
        Some(r) => r,
        None => {
            warn!("array callback: index {} out of bounds", callback_idx);
            return 0;
        }
    };

    let func = match func_ref {
        Ref::Func(Some(f)) => f,
        _ => {
            warn!("array callback: null/invalid funcref at index {}", callback_idx);
            return 0;
        }
    };

    let ty = func.ty(&*caller);
    let params: Vec<Val> = args.iter().copied().map(Val::I32).collect();
    let n_results = ty.results().len();
    let mut results: Vec<Val> = (0..n_results).map(|_| Val::I32(0)).collect();

    if let Err(e) = func.call(&mut *caller, &params, &mut results) {
        warn!("array callback {} trapped: {}", callback_idx, e);
        return 0;
    }

    // Expect a single i32 result; coerce other types to 0 conservatively.
    match results.first() {
        Some(Val::I32(v)) => *v,
        Some(Val::I64(v)) => *v as i32,
        Some(Val::F32(v)) => f32::from_bits(*v) as i32,
        Some(Val::F64(v)) => *v as i32,
        _ => 0,
    }
}

/// Register all array_* functions.
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // array_get(arr, idx) -> i32 — 0 on invalid handle or OOB
    linker.func_wrap("env", "array_get",
        |_: Caller<'_, S>, arr: i32, idx: i32| -> i32 {
            with_array(arr, |v| {
                if idx < 0 { return 0; }
                v.get(idx as usize).copied().unwrap_or(0)
            }).unwrap_or(0)
        })?;

    // array_set(arr, idx, value) -> void
    linker.func_wrap("env", "array_set",
        |_: Caller<'_, S>, arr: i32, idx: i32, value: i32| {
            let _ = with_array_mut(arr, |v| {
                if idx >= 0 && (idx as usize) < v.len() {
                    v[idx as usize] = value;
                }
            });
        })?;

    // array_push(arr, value) -> i32 — returns the same handle
    linker.func_wrap("env", "array_push",
        |_: Caller<'_, S>, arr: i32, value: i32| -> i32 {
            with_array_mut(arr, |v| v.push(value)).unwrap_or(());
            arr
        })?;

    // array_pop(arr) -> i32 — popped value, 0 if empty/invalid
    linker.func_wrap("env", "array_pop",
        |_: Caller<'_, S>, arr: i32| -> i32 {
            with_array_mut(arr, |v| v.pop().unwrap_or(0)).unwrap_or(0)
        })?;

    // array_slice(arr, start, end) -> new handle
    linker.func_wrap("env", "array_slice",
        |_: Caller<'_, S>, arr: i32, start: i32, end: i32| -> i32 {
            let sliced = with_array(arr, |v| {
                let len = v.len() as i32;
                let s = start.clamp(0, len) as usize;
                let e = end.clamp(0, len).max(start.clamp(0, len)) as usize;
                v[s..e].to_vec()
            }).unwrap_or_default();
            store_array(sliced)
        })?;

    // array_concat(a, b) -> new handle
    linker.func_wrap("env", "array_concat",
        |_: Caller<'_, S>, a: i32, b: i32| -> i32 {
            let mut combined = Vec::new();
            with_array(a, |v| combined.extend_from_slice(v));
            with_array(b, |v| combined.extend_from_slice(v));
            store_array(combined)
        })?;

    // array_reverse(arr) -> new handle (matches node-server: non-mutating)
    linker.func_wrap("env", "array_reverse",
        |_: Caller<'_, S>, arr: i32| -> i32 {
            let mut reversed = with_array(arr, |v| v.clone()).unwrap_or_default();
            reversed.reverse();
            store_array(reversed)
        })?;

    // array_sort(arr) -> new handle (ascending)
    linker.func_wrap("env", "array_sort",
        |_: Caller<'_, S>, arr: i32| -> i32 {
            let mut sorted = with_array(arr, |v| v.clone()).unwrap_or_default();
            sorted.sort();
            store_array(sorted)
        })?;

    // array_contains(arr, value) -> boolean
    linker.func_wrap("env", "array_contains",
        |_: Caller<'_, S>, arr: i32, value: i32| -> i32 {
            with_array(arr, |v| if v.contains(&value) { 1 } else { 0 }).unwrap_or(0)
        })?;

    // array_filter(arr, callback_idx) -> new handle of elements where cb(e) != 0
    linker.func_wrap("env", "array_filter",
        |mut caller: Caller<'_, S>, arr: i32, callback_idx: i32| -> i32 {
            let snapshot = with_array(arr, |v| v.clone()).unwrap_or_default();
            let mut out = Vec::with_capacity(snapshot.len());
            for elem in snapshot {
                if call_indirect(&mut caller, callback_idx, &[elem]) != 0 {
                    out.push(elem);
                }
            }
            store_array(out)
        })?;

    // array_map(arr, callback_idx) -> new handle of cb(e) for each e
    linker.func_wrap("env", "array_map",
        |mut caller: Caller<'_, S>, arr: i32, callback_idx: i32| -> i32 {
            let snapshot = with_array(arr, |v| v.clone()).unwrap_or_default();
            let mut out = Vec::with_capacity(snapshot.len());
            for elem in snapshot {
                out.push(call_indirect(&mut caller, callback_idx, &[elem]));
            }
            store_array(out)
        })?;

    // array_reduce(arr, callback_idx, initial) -> i32
    linker.func_wrap("env", "array_reduce",
        |mut caller: Caller<'_, S>, arr: i32, callback_idx: i32, initial: i32| -> i32 {
            let snapshot = with_array(arr, |v| v.clone()).unwrap_or_default();
            let mut acc = initial;
            for elem in snapshot {
                acc = call_indirect(&mut caller, callback_idx, &[acc, elem]);
            }
            acc
        })?;

    // array_find(arr, callback_idx) -> first element where cb(e) != 0, else 0
    linker.func_wrap("env", "array_find",
        |mut caller: Caller<'_, S>, arr: i32, callback_idx: i32| -> i32 {
            let snapshot = with_array(arr, |v| v.clone()).unwrap_or_default();
            for elem in snapshot {
                if call_indirect(&mut caller, callback_idx, &[elem]) != 0 {
                    return elem;
                }
            }
            0
        })?;

    debug!("array_funcs: registered 13 functions, callback dispatch via __indirect_function_table");
    Ok(())
}
