//! Memory Helpers for WASM Host Functions
//!
//! Provides utilities for reading and writing data to/from WASM linear memory.
//! Clean Language uses length-prefixed strings: [4-byte little-endian length][UTF-8 data]
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::state::WasmStateCore;
use tracing::{debug, error};
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

    debug!("read_raw_string: ptr={}, len={}, start={}, end={}, memory_size={}", ptr, len, start, end, data.len());

    if end > data.len() {
        error!("read_raw_string: out of bounds: {}..{} (memory size: {})", start, end, data.len());
        return None;
    }

    match std::str::from_utf8(&data[start..end]) {
        Ok(s) => {
            debug!("read_raw_string: successfully read '{}' ({} bytes)",
                   if s.len() > 100 { format!("{}...", &s[..100]) } else { s.to_string() },
                   s.len());
            Some(s.to_string())
        }
        Err(e) => {
            error!("read_raw_string: UTF-8 conversion failed: {}", e);
            None
        }
    }
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
///
/// This function uses WASM's malloc to allocate memory, ensuring the allocation
/// is properly tracked by WASM's memory management. This is critical because
/// WASM's allocator uses a global variable for the heap pointer, not linear memory.
pub fn write_string_to_caller<S: WasmStateCore>(caller: &mut Caller<'_, S>, s: &str) -> i32 {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let total_size = STRING_LENGTH_PREFIX_SIZE + len;

    error!("write_string_to_caller: Writing string ({} bytes)", len);

    // MUST use WASM's malloc to allocate - updating memory[0] doesn't work
    // because WASM's allocator uses a global variable, not linear memory.
    let ptr = if let Some(malloc) = caller.get_export("malloc").and_then(|e| e.into_func()) {
        error!("write_string_to_caller: Found malloc export, calling WASM malloc({})", total_size);
        match malloc.typed::<i32, i32>(&*caller) {
            Ok(typed_malloc) => {
                match typed_malloc.call(&mut *caller, total_size as i32) {
                    Ok(p) if p > 0 => {
                        error!("write_string_to_caller: WASM malloc({}) returned ptr={}", total_size, p);
                        p as usize
                    }
                    Ok(p) => {
                        error!("write_string_to_caller: WASM malloc returned invalid ptr={}", p);
                        return 0;
                    }
                    Err(e) => {
                        error!("write_string_to_caller: WASM malloc call failed: {}", e);
                        return 0;
                    }
                }
            }
            Err(e) => {
                error!("write_string_to_caller: malloc type mismatch: {}", e);
                return 0;
            }
        }
    } else {
        error!("write_string_to_caller: No malloc export found");
        return 0;
    };

    // Re-acquire memory reference after malloc call - malloc might have grown memory
    // and we need the updated memory view
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => {
            error!("write_string_to_caller: No memory export found after malloc");
            return 0;
        }
    };

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

    error!("write_string_to_caller: Successfully wrote {} bytes at ptr={}", len, ptr);
    ptr as i32
}

/// Write bytes to WASM memory with length prefix
/// CRITICAL: Uses WASM malloc to allocate memory, ensuring coordination with WASM's heap.
/// Previously used allocate_at_memory_end which caused memory corruption when overlapping
/// with WASM's heap (State allocator starts at 65536, WASM heap grows from ~1024).
pub fn write_bytes_to_caller<S: WasmStateCore>(caller: &mut Caller<'_, S>, bytes: &[u8]) -> i32 {
    let len = bytes.len();
    let total_size = STRING_LENGTH_PREFIX_SIZE + len;

    error!("write_bytes_to_caller: Writing {} bytes", len);

    // MUST use WASM's malloc to allocate - using State allocator causes memory overlap
    // because WASM's allocator uses a global variable that grows from ~1024 upward,
    // while State's allocator starts at 65536 and grows upward - they can overlap!
    let ptr = if let Some(malloc) = caller.get_export("malloc").and_then(|e| e.into_func()) {
        error!("write_bytes_to_caller: Found malloc, calling WASM malloc({})", total_size);
        match malloc.typed::<i32, i32>(&*caller) {
            Ok(typed_malloc) => {
                match typed_malloc.call(&mut *caller, total_size as i32) {
                    Ok(p) if p > 0 => {
                        error!("write_bytes_to_caller: WASM malloc({}) returned ptr={}", total_size, p);
                        p as usize
                    }
                    Ok(p) => {
                        error!("write_bytes_to_caller: WASM malloc returned invalid ptr={}", p);
                        return 0;
                    }
                    Err(e) => {
                        error!("write_bytes_to_caller: WASM malloc call failed: {}", e);
                        return 0;
                    }
                }
            }
            Err(e) => {
                error!("write_bytes_to_caller: malloc type mismatch: {}", e);
                return 0;
            }
        }
    } else {
        error!("write_bytes_to_caller: No malloc export found");
        return 0;
    };

    // Re-acquire memory reference after malloc call - malloc might have grown memory
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => {
            error!("write_bytes_to_caller: No memory export found after malloc");
            return 0;
        }
    };

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

    error!("write_bytes_to_caller: Successfully wrote {} bytes at ptr={}", len, ptr);
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

/// Allocate memory by updating WASM's heap pointer directly
///
/// This function allocates memory in a way that's compatible with WASM's
/// internal allocator. It works by:
/// 1. Reading the current heap pointer from memory address 0
/// 2. Allocating at that location
/// 3. Updating the heap pointer to skip past the allocation
/// 4. Growing memory if needed
///
/// This ensures host allocations are properly tracked and won't conflict
/// with subsequent WASM allocations.
pub fn allocate_in_fresh_memory<S: WasmStateCore>(caller: &mut Caller<'_, S>, memory: &wasmtime::Memory, size: usize) -> usize {
    // Read current WASM heap pointer (stored at address 0)
    let mut heap_ptr_bytes = [0u8; 4];
    if memory.read(&*caller, 0, &mut heap_ptr_bytes).is_err() {
        error!("allocate_in_fresh_memory: Failed to read heap pointer");
        return 0;
    }
    let current_heap_ptr = u32::from_le_bytes(heap_ptr_bytes) as usize;

    // Align allocation to 8 bytes for safety
    let aligned_size = (size + 7) & !7;

    // Allocate at the current heap pointer location
    let ptr = current_heap_ptr;
    let new_heap_ptr = ptr + aligned_size;

    // Ensure memory is large enough
    let current_size = memory.data_size(&*caller);
    if new_heap_ptr > current_size {
        // Calculate required pages (64KB per page)
        let required_pages = ((new_heap_ptr + 65535) / 65536) as u64;
        let current_pages = memory.size(&*caller);
        let pages_to_grow = required_pages.saturating_sub(current_pages);

        if pages_to_grow > 0 {
            match memory.grow(&mut *caller, pages_to_grow) {
                Ok(_) => {
                    debug!("allocate_in_fresh_memory: Grew memory by {} pages", pages_to_grow);
                }
                Err(e) => {
                    error!("allocate_in_fresh_memory: Failed to grow memory: {}", e);
                    return 0;
                }
            }
        }
    }

    // Update WASM's heap pointer to skip past our allocation
    let new_heap_ptr_bytes = (new_heap_ptr as u32).to_le_bytes();
    if memory.write(&mut *caller, 0, &new_heap_ptr_bytes).is_err() {
        error!("allocate_in_fresh_memory: Failed to update heap pointer");
        return 0;
    }

    debug!("allocate_in_fresh_memory: Allocated {} bytes at ptr={}", size, ptr);
    ptr
}

/// Allocate memory at the end of WASM linear memory
///
/// Uses the state's bump allocator and grows memory if needed.
/// NOTE: This uses a fixed-offset allocator which may conflict with WASM's heap.
/// For host-returned strings, prefer allocate_in_fresh_memory() instead.
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
