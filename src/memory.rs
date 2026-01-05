//! WASM Memory Management
//!
//! Handles reading/writing strings and data between Rust and WASM memory.
//! Clean Language uses length-prefixed strings: [4-byte length][UTF-8 data]

use crate::error::{RuntimeError, RuntimeResult};
use wasmtime::{Caller, Memory, Store};

/// Clean string format: [4-byte little-endian length][UTF-8 bytes]
pub const STRING_LENGTH_PREFIX_SIZE: usize = 4;

/// Minimum allocation alignment (8 bytes)
pub const ALIGNMENT: usize = 8;

/// Memory manager for WASM instance
pub struct WasmMemory {
    /// Current allocation offset (bump allocator)
    offset: usize,
}

impl WasmMemory {
    /// Create a new memory manager
    pub fn new() -> Self {
        Self {
            // Start allocation after initial memory region to avoid
            // overwriting WASM's own data structures
            offset: 65536, // Start at 64KB
        }
    }

    /// Allocate memory and return the pointer
    pub fn allocate(&mut self, size: usize) -> usize {
        let ptr = self.offset;
        // Align to 8 bytes for safety
        self.offset = (self.offset + size + ALIGNMENT - 1) & !(ALIGNMENT - 1);
        ptr
    }

    /// Reset allocator (for between requests)
    pub fn reset(&mut self) {
        self.offset = 65536;
    }

    /// Get current allocation offset
    pub fn current_offset(&self) -> usize {
        self.offset
    }
}

impl Default for WasmMemory {
    fn default() -> Self {
        Self::new()
    }
}

/// Write a Clean Language string to WASM memory
/// Returns the pointer to the string in WASM memory
pub fn write_string_to_memory<T>(
    store: &mut Store<T>,
    memory: &Memory,
    allocator: &mut WasmMemory,
    s: &str,
) -> RuntimeResult<u32> {
    let bytes = s.as_bytes();
    let total_size = STRING_LENGTH_PREFIX_SIZE + bytes.len();

    // Allocate memory
    let ptr = allocator.allocate(total_size);

    // Ensure memory is large enough
    ensure_memory_size(store, memory, ptr + total_size)?;

    // Write length prefix (little-endian u32)
    let len_bytes = (bytes.len() as u32).to_le_bytes();
    memory
        .write(&mut *store, ptr, &len_bytes)
        .map_err(|e| RuntimeError::memory(format!("Failed to write string length: {}", e)))?;

    // Write string data
    memory
        .write(&mut *store, ptr + STRING_LENGTH_PREFIX_SIZE, bytes)
        .map_err(|e| RuntimeError::memory(format!("Failed to write string data: {}", e)))?;

    Ok(ptr as u32)
}

/// Read a Clean Language string from WASM memory
pub fn read_string_from_memory<T>(
    store: &Store<T>,
    memory: &Memory,
    ptr: u32,
) -> RuntimeResult<String> {
    let data = memory.data(store);
    let ptr = ptr as usize;

    // Check bounds for length prefix
    if ptr + STRING_LENGTH_PREFIX_SIZE > data.len() {
        return Err(RuntimeError::memory(format!(
            "String pointer {} out of bounds (memory size: {})",
            ptr,
            data.len()
        )));
    }

    // Read length
    let len_bytes: [u8; 4] = data[ptr..ptr + STRING_LENGTH_PREFIX_SIZE]
        .try_into()
        .map_err(|_| RuntimeError::memory("Failed to read string length"))?;
    let len = u32::from_le_bytes(len_bytes) as usize;

    // Check bounds for string data
    let data_start = ptr + STRING_LENGTH_PREFIX_SIZE;
    let data_end = data_start + len;

    if data_end > data.len() {
        return Err(RuntimeError::memory(format!(
            "String data out of bounds: {}..{} (memory size: {})",
            data_start,
            data_end,
            data.len()
        )));
    }

    // Read and convert to string
    std::str::from_utf8(&data[data_start..data_end])
        .map(|s| s.to_string())
        .map_err(|e| RuntimeError::memory(format!("Invalid UTF-8 in string: {}", e)))
}

/// Read a Clean Language string from WASM memory (caller version)
pub fn read_string_from_caller<T>(caller: &mut Caller<'_, T>, ptr: u32) -> RuntimeResult<String> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| RuntimeError::memory("No memory export found"))?;

    let data = memory.data(&*caller);
    let ptr = ptr as usize;

    // Check bounds for length prefix
    if ptr + STRING_LENGTH_PREFIX_SIZE > data.len() {
        return Err(RuntimeError::memory(format!(
            "String pointer {} out of bounds (memory size: {})",
            ptr,
            data.len()
        )));
    }

    // Read length
    let len_bytes: [u8; 4] = data[ptr..ptr + STRING_LENGTH_PREFIX_SIZE]
        .try_into()
        .map_err(|_| RuntimeError::memory("Failed to read string length"))?;
    let len = u32::from_le_bytes(len_bytes) as usize;

    // Check bounds for string data
    let data_start = ptr + STRING_LENGTH_PREFIX_SIZE;
    let data_end = data_start + len;

    if data_end > data.len() {
        return Err(RuntimeError::memory(format!(
            "String data out of bounds: {}..{} (memory size: {})",
            data_start,
            data_end,
            data.len()
        )));
    }

    // Read and convert to string
    std::str::from_utf8(&data[data_start..data_end])
        .map(|s| s.to_string())
        .map_err(|e| RuntimeError::memory(format!("Invalid UTF-8 in string: {}", e)))
}

/// Write bytes directly to WASM memory
pub fn write_bytes_to_memory<T>(
    store: &mut Store<T>,
    memory: &Memory,
    allocator: &mut WasmMemory,
    bytes: &[u8],
) -> RuntimeResult<u32> {
    let ptr = allocator.allocate(bytes.len());
    ensure_memory_size(store, memory, ptr + bytes.len())?;

    memory
        .write(store, ptr, bytes)
        .map_err(|e| RuntimeError::memory(format!("Failed to write bytes: {}", e)))?;

    Ok(ptr as u32)
}

/// Read bytes from WASM memory
pub fn read_bytes_from_memory<T>(
    store: &Store<T>,
    memory: &Memory,
    ptr: u32,
    len: u32,
) -> RuntimeResult<Vec<u8>> {
    let data = memory.data(store);
    let start = ptr as usize;
    let end = start + len as usize;

    if end > data.len() {
        return Err(RuntimeError::memory(format!(
            "Bytes out of bounds: {}..{} (memory size: {})",
            start,
            end,
            data.len()
        )));
    }

    Ok(data[start..end].to_vec())
}

/// Ensure WASM memory is at least the specified size
fn ensure_memory_size<T>(
    store: &mut Store<T>,
    memory: &Memory,
    required: usize,
) -> RuntimeResult<()> {
    let current_size = memory.data_size(&*store);

    if required > current_size {
        // Calculate required pages (64KB per page)
        let required_pages = ((required + 65535) / 65536) as u64;
        let current_pages = memory.size(&*store);
        let pages_to_grow = required_pages.saturating_sub(current_pages);

        if pages_to_grow > 0 {
            memory.grow(&mut *store, pages_to_grow).map_err(|e| {
                RuntimeError::memory(format!(
                    "Failed to grow memory by {} pages: {}",
                    pages_to_grow, e
                ))
            })?;
        }
    }

    Ok(())
}

/// Write an i32 to WASM memory at the given pointer
pub fn write_i32<T>(
    store: &mut Store<T>,
    memory: &Memory,
    ptr: u32,
    value: i32,
) -> RuntimeResult<()> {
    let bytes = value.to_le_bytes();
    memory
        .write(store, ptr as usize, &bytes)
        .map_err(|e| RuntimeError::memory(format!("Failed to write i32: {}", e)))
}

/// Read an i32 from WASM memory
pub fn read_i32<T>(store: &Store<T>, memory: &Memory, ptr: u32) -> RuntimeResult<i32> {
    let data = memory.data(store);
    let start = ptr as usize;

    if start + 4 > data.len() {
        return Err(RuntimeError::memory("i32 read out of bounds"));
    }

    let bytes: [u8; 4] = data[start..start + 4]
        .try_into()
        .map_err(|_| RuntimeError::memory("Failed to read i32"))?;

    Ok(i32::from_le_bytes(bytes))
}

/// Write a JSON value as a string to WASM memory
pub fn write_json_to_memory<T>(
    store: &mut Store<T>,
    memory: &Memory,
    allocator: &mut WasmMemory,
    value: &serde_json::Value,
) -> RuntimeResult<u32> {
    let json_str = serde_json::to_string(value)
        .map_err(|e| RuntimeError::memory(format!("Failed to serialize JSON: {}", e)))?;
    write_string_to_memory(store, memory, allocator, &json_str)
}

/// Read a JSON value from WASM memory
pub fn read_json_from_memory<T>(
    store: &Store<T>,
    memory: &Memory,
    ptr: u32,
) -> RuntimeResult<serde_json::Value> {
    let json_str = read_string_from_memory(store, memory, ptr)?;
    serde_json::from_str(&json_str)
        .map_err(|e| RuntimeError::memory(format!("Failed to parse JSON: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocator() {
        let mut mem = WasmMemory::new();

        let ptr1 = mem.allocate(100);
        assert_eq!(ptr1, 65536);

        let ptr2 = mem.allocate(200);
        // 65536 + 100 = 65636, aligned to 8 = 65640
        assert_eq!(ptr2, 65640);

        mem.reset();
        let ptr3 = mem.allocate(50);
        assert_eq!(ptr3, 65536);
    }

    #[test]
    fn test_alignment() {
        let mut mem = WasmMemory::new();

        // Allocate 1 byte
        let ptr1 = mem.allocate(1);
        assert_eq!(ptr1, 65536);

        // Next allocation should be aligned
        let ptr2 = mem.allocate(1);
        assert_eq!(ptr2, 65544); // 65536 + 8 (aligned)
    }
}
