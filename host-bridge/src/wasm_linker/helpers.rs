//! Memory Helpers for WASM Host Functions
//!
//! Provides utilities for reading and writing data to/from WASM linear memory.
//! Clean Language uses length-prefixed strings: [4-byte little-endian length][UTF-8 data]
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::state::WasmStateCore;
use tracing::{debug, error, warn};
use wasmtime::Caller;

/// Clean string format: [4-byte little-endian length][UTF-8 bytes]
pub const STRING_LENGTH_PREFIX_SIZE: usize = 4;

/// Read a Clean Language string from WASM memory
///
/// The string format is: [4-byte little-endian length][UTF-8 bytes]
pub fn read_string_from_caller<S: WasmStateCore>(caller: &mut Caller<'_, S>, ptr: i32) -> Option<String> {
    let memory = caller.get_export("memory").and_then(|e| e.into_memory())?;
    let data = memory.data(&*caller);
    let ptr = ptr as usize;

    // Check bounds for length prefix
    if ptr + STRING_LENGTH_PREFIX_SIZE > data.len() {
        error!("read_string_from_caller: ptr {} out of bounds (memory size: {})", ptr, data.len());
        return None;
    }

    // Read length
    let len_bytes: [u8; 4] = data[ptr..ptr + STRING_LENGTH_PREFIX_SIZE].try_into().ok()?;
    let len = u32::from_le_bytes(len_bytes) as usize;

    // Check bounds for string data
    let data_start = ptr + STRING_LENGTH_PREFIX_SIZE;
    let data_end = data_start + len;

    if data_end > data.len() {
        error!("read_string_from_caller: string data out of bounds: {}..{} (memory size: {})",
               data_start, data_end, data.len());
        return None;
    }

    // Read and convert to string
    std::str::from_utf8(&data[data_start..data_end])
        .map(|s| s.to_string())
        .ok()
}

/// Read a raw string from WASM memory (no length prefix, uses explicit length)
pub fn read_raw_string<S: WasmStateCore>(caller: &mut Caller<'_, S>, ptr: i32, len: i32) -> Option<String> {
    let memory = caller.get_export("memory").and_then(|e| e.into_memory())?;
    let data = memory.data(&*caller);

    let start = ptr as usize;
    let end = start + len as usize;

    if end > data.len() {
        error!("read_raw_string: out of bounds: {}..{} (memory size: {})", start, end, data.len());
        return None;
    }

    std::str::from_utf8(&data[start..end])
        .map(|s| s.to_string())
        .ok()
}

/// Read raw bytes from WASM memory (no length prefix, uses explicit length)
pub fn read_raw_bytes<S: WasmStateCore>(caller: &mut Caller<'_, S>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    let memory = caller.get_export("memory").and_then(|e| e.into_memory())?;
    let data = memory.data(&*caller);

    let start = ptr as usize;
    let end = start + len as usize;

    if end > data.len() {
        error!("read_raw_bytes: out of bounds: {}..{} (memory size: {})", start, end, data.len());
        return None;
    }

    Some(data[start..end].to_vec())
}

/// Write a Clean Language string to WASM memory
///
/// Returns the pointer to the written string, or 0 on failure.
/// Uses WASM's malloc if available, otherwise falls back to bump allocator.
pub fn write_string_to_caller<S: WasmStateCore>(caller: &mut Caller<'_, S>, s: &str) -> i32 {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let total_size = STRING_LENGTH_PREFIX_SIZE + len;

    debug!("write_string_to_caller: Writing '{}' ({} bytes, total_size={})", s, len, total_size);

    // Get WASM memory
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => {
            error!("write_string_to_caller: No memory export found");
            return 0;
        }
    };

    // Try to use WASM's malloc function to allocate memory
    let ptr = if let Some(malloc) = caller.get_export("malloc").and_then(|e| e.into_func()) {
        match malloc.typed::<i32, i32>(&*caller) {
            Ok(typed_malloc) => {
                match typed_malloc.call(&mut *caller, total_size as i32) {
                    Ok(p) if p > 0 => {
                        debug!("write_string_to_caller: WASM malloc returned ptr={}", p);
                        p as usize
                    }
                    Ok(_) => {
                        warn!("write_string_to_caller: WASM malloc returned 0, falling back to host allocator");
                        allocate_at_memory_end(caller, &memory, total_size)
                    }
                    Err(e) => {
                        warn!("write_string_to_caller: WASM malloc failed: {}, falling back to host allocator", e);
                        allocate_at_memory_end(caller, &memory, total_size)
                    }
                }
            }
            Err(_) => {
                debug!("write_string_to_caller: malloc type mismatch, using host allocator");
                allocate_at_memory_end(caller, &memory, total_size)
            }
        }
    } else {
        debug!("write_string_to_caller: No malloc export, using host allocator");
        allocate_at_memory_end(caller, &memory, total_size)
    };

    if ptr == 0 {
        error!("write_string_to_caller: Failed to allocate {} bytes", total_size);
        return 0;
    }

    debug!("write_string_to_caller: Writing to ptr={}, memory_size={}", ptr, memory.data_size(&*caller));

    // Write length prefix (4 bytes, little-endian)
    let len_bytes = (len as u32).to_le_bytes();
    if let Err(e) = memory.write(&mut *caller, ptr, &len_bytes) {
        error!("write_string_to_caller: Failed to write length at ptr={}: {}", ptr, e);
        return 0;
    }

    // Write string data
    if let Err(e) = memory.write(&mut *caller, ptr + STRING_LENGTH_PREFIX_SIZE, bytes) {
        error!("write_string_to_caller: Failed to write string data at ptr={}: {}", ptr + STRING_LENGTH_PREFIX_SIZE, e);
        return 0;
    }

    debug!("write_string_to_caller: Successfully wrote string at ptr={}", ptr);
    ptr as i32
}

/// Write bytes to WASM memory with length prefix
pub fn write_bytes_to_caller<S: WasmStateCore>(caller: &mut Caller<'_, S>, bytes: &[u8]) -> i32 {
    let len = bytes.len();
    let total_size = STRING_LENGTH_PREFIX_SIZE + len;

    // Get WASM memory
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => {
            error!("write_bytes_to_caller: No memory export found");
            return 0;
        }
    };

    // Allocate memory
    let ptr = allocate_at_memory_end(caller, &memory, total_size);
    if ptr == 0 {
        return 0;
    }

    // Write length prefix
    let len_bytes = (len as u32).to_le_bytes();
    if let Err(e) = memory.write(&mut *caller, ptr, &len_bytes) {
        error!("write_bytes_to_caller: Failed to write length: {}", e);
        return 0;
    }

    // Write data
    if let Err(e) = memory.write(&mut *caller, ptr + STRING_LENGTH_PREFIX_SIZE, bytes) {
        error!("write_bytes_to_caller: Failed to write data: {}", e);
        return 0;
    }

    ptr as i32
}

/// Read a length-prefixed byte array from WASM memory (from raw data slice)
pub fn read_length_prefixed_bytes(data: &[u8], ptr: usize) -> Vec<u8> {
    if ptr + STRING_LENGTH_PREFIX_SIZE > data.len() {
        return Vec::new();
    }

    let len_bytes: [u8; 4] = match data[ptr..ptr + STRING_LENGTH_PREFIX_SIZE].try_into() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let len = u32::from_le_bytes(len_bytes) as usize;

    let data_start = ptr + STRING_LENGTH_PREFIX_SIZE;
    let data_end = data_start + len;

    if data_end > data.len() {
        return Vec::new();
    }

    data[data_start..data_end].to_vec()
}

/// Allocate memory at the end of WASM linear memory
///
/// Uses the state's bump allocator and grows memory if needed.
pub fn allocate_at_memory_end<S: WasmStateCore>(caller: &mut Caller<'_, S>, memory: &wasmtime::Memory, size: usize) -> usize {
    // Get current allocation offset from state
    let state = caller.data_mut();
    let ptr = state.memory_mut().allocate(size);

    // Ensure memory is large enough
    let required = ptr + size;
    let current_size = memory.data_size(&*caller);

    debug!("allocate_at_memory_end: ptr={}, size={}, required={}, current_size={}", ptr, size, required, current_size);

    if required > current_size {
        // Calculate required pages (64KB per page)
        let required_pages = ((required + 65535) / 65536) as u64;
        let current_pages = memory.size(&*caller);
        let pages_to_grow = required_pages.saturating_sub(current_pages);

        debug!("allocate_at_memory_end: need to grow from {} to {} pages ({} new pages)",
               current_pages, required_pages, pages_to_grow);

        if pages_to_grow > 0 {
            match memory.grow(&mut *caller, pages_to_grow) {
                Ok(prev_pages) => {
                    debug!("allocate_at_memory_end: grew from {} to {} pages", prev_pages, prev_pages + pages_to_grow);
                }
                Err(e) => {
                    error!("allocate_at_memory_end: Failed to grow memory by {} pages: {}", pages_to_grow, e);
                    return 0;
                }
            }
        }
    }

    ptr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_length_prefixed_bytes() {
        // Create test data: length prefix (5) + "hello"
        let mut data = vec![5, 0, 0, 0]; // length = 5 (little endian)
        data.extend_from_slice(b"hello");

        let result = read_length_prefixed_bytes(&data, 0);
        assert_eq!(result, b"hello");
    }

    #[test]
    fn test_read_length_prefixed_bytes_empty() {
        // Empty string
        let data = vec![0, 0, 0, 0]; // length = 0
        let result = read_length_prefixed_bytes(&data, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_length_prefixed_bytes_out_of_bounds() {
        let data = vec![10, 0, 0, 0]; // length = 10, but no data
        let result = read_length_prefixed_bytes(&data, 0);
        assert!(result.is_empty()); // Should return empty on out of bounds
    }
}
