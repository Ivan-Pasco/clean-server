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
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

const LIST_HEADER_SIZE: usize = 16;
const F64_SIZE: usize = 8;

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
            let length = u32::from_le_bytes(
                data[ptr..ptr + 4].try_into().unwrap()
            ) as usize;

            // Read capacity
            let capacity = u32::from_le_bytes(
                data[ptr + 4..ptr + 8].try_into().unwrap()
            ) as usize;

            debug!("list.push_f64: ptr={}, length={}, capacity={}, value={}", ptr, length, capacity, value);

            // Check there's room (capacity must be > length)
            if length >= capacity {
                error!("list.push_f64: list is full (length={}, capacity={})", length, capacity);
                return list_ptr;
            }

            // Calculate element offset
            let element_offset = ptr + LIST_HEADER_SIZE + length * F64_SIZE;

            // Bounds check for the new element
            if element_offset + F64_SIZE > data.len() {
                error!("list.push_f64: element write out of bounds at offset={}", element_offset);
                return list_ptr;
            }

            // Write the f64 value
            let value_bytes = value.to_le_bytes();
            let data_mut = memory.data_mut(&mut caller);
            data_mut[element_offset..element_offset + F64_SIZE].copy_from_slice(&value_bytes);

            // Increment length
            let new_length = (length + 1) as u32;
            data_mut[ptr..ptr + 4].copy_from_slice(&new_length.to_le_bytes());

            debug!("list.push_f64: pushed value={} at index={}, new_length={}", value, length, new_length);
            list_ptr
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests would require WASM runtime setup
}
