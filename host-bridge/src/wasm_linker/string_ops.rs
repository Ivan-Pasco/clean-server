//! String Allocation Host Functions
//!
//! Provides string operations that require memory allocation:
//! - concat, substring, trim, toUpper, toLower, replace, split
//! - Type conversions that produce strings: int_to_string, float_to_string, bool_to_string
//!
//! String format: [4-byte little-endian length][UTF-8 bytes]
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{
    read_string_from_caller, read_raw_string, write_string_to_caller,
    write_bytes_to_caller, read_length_prefixed_bytes,
};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use tracing::error;
use wasmtime::{Caller, Linker};

/// Register all string operation functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // STRING CONCATENATION
    // =========================================

    // string_concat - Concatenate two raw strings (with explicit lengths)
    linker.func_wrap(
        "env",
        "string_concat",
        |mut caller: Caller<'_, S>, ptr1: i32, len1: i32, ptr2: i32, len2: i32| -> i32 {
            let s1 = read_raw_string(&mut caller, ptr1, len1).unwrap_or_default();
            let s2 = read_raw_string(&mut caller, ptr2, len2).unwrap_or_default();
            let result = format!("{}{}", s1, s2);
            write_string_to_caller(&mut caller, &result)
        },
    )?;

    // string.concat - Concatenate two length-prefixed strings
    // IMPORTANT: Use write_string_to_caller (WASM malloc) for consistency with WASM heap
    linker.func_wrap(
        "env",
        "string.concat",
        |mut caller: Caller<'_, S>, ptr1: i32, ptr2: i32| -> i32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };
            let data = memory.data(&caller);

            // Read both strings
            let bytes1 = read_length_prefixed_bytes(data, ptr1 as usize);
            let bytes2 = read_length_prefixed_bytes(data, ptr2 as usize);

            // Concatenate bytes
            let mut result = bytes1.clone();
            result.extend(&bytes2);

            // Debug: Log the concat operation
            let s1_preview = std::str::from_utf8(&bytes1).unwrap_or("(invalid utf8)");
            let s2_preview = std::str::from_utf8(&bytes2).unwrap_or("(invalid utf8)");
            error!("string.concat: ptr1={} ('{}'), ptr2={} ('{}'), result_len={}",
                   ptr1,
                   if s1_preview.len() > 50 { &s1_preview[..50] } else { s1_preview },
                   ptr2,
                   if s2_preview.len() > 50 { &s2_preview[..50] } else { s2_preview },
                   result.len());

            // Convert to string and use write_string_to_caller for WASM malloc consistency
            let output_ptr = match std::str::from_utf8(&result) {
                Ok(s) => write_string_to_caller(&mut caller, s),
                Err(_) => write_bytes_to_caller(&mut caller, &result),
            };

            error!("string.concat: output_ptr={}, total_size={}", output_ptr, result.len() + 4);

            output_ptr
        },
    )?;

    // =========================================
    // STRING MANIPULATION
    // =========================================

    // string_substring - Extract substring
    linker.func_wrap(
        "env",
        "string_substring",
        |mut caller: Caller<'_, S>, str_ptr: i32, start: i32, end: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let start = start.max(0) as usize;
            let end = end.max(0) as usize;

            let result = if start < s.len() && end <= s.len() && start <= end {
                &s[start..end]
            } else if start < s.len() {
                &s[start..]
            } else {
                ""
            };

            write_string_to_caller(&mut caller, result)
        },
    )?;

    // string.substring - Alias
    linker.func_wrap(
        "env",
        "string.substring",
        |mut caller: Caller<'_, S>, str_ptr: i32, start: i32, end: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let start = start.max(0) as usize;
            let end = end.max(0) as usize;

            let result = if start < s.len() && end <= s.len() && start <= end {
                &s[start..end]
            } else if start < s.len() {
                &s[start..]
            } else {
                ""
            };

            write_string_to_caller(&mut caller, result)
        },
    )?;

    // string_trim - Trim whitespace from both ends
    linker.func_wrap(
        "env",
        "string_trim",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.trim",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim())
        },
    )?;

    // string.trimStart - Trim leading whitespace
    linker.func_wrap(
        "env",
        "string.trimStart",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim_start())
        },
    )?;

    // string.trimEnd - Trim trailing whitespace
    linker.func_wrap(
        "env",
        "string.trimEnd",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim_end())
        },
    )?;

    // string_to_upper / string.toUpperCase - Convert to uppercase
    linker.func_wrap(
        "env",
        "string_to_upper",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_uppercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.toUpperCase",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_uppercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string_toUpperCase",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_uppercase())
        },
    )?;

    // string_to_lower / string.toLowerCase - Convert to lowercase
    linker.func_wrap(
        "env",
        "string_to_lower",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_lowercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.toLowerCase",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_lowercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string_toLowerCase",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_lowercase())
        },
    )?;

    // string_replace / string.replace - Replace first occurrence
    linker.func_wrap(
        "env",
        "string_replace",
        |mut caller: Caller<'_, S>, str_ptr: i32, search_ptr: i32, replace_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let search = read_string_from_caller(&mut caller, search_ptr).unwrap_or_default();
            let replace = read_string_from_caller(&mut caller, replace_ptr).unwrap_or_default();

            let result = s.replacen(&search, &replace, 1);
            write_string_to_caller(&mut caller, &result)
        },
    )?;

    linker.func_wrap(
        "env",
        "string.replace",
        |mut caller: Caller<'_, S>, str_ptr: i32, search_ptr: i32, replace_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let search = read_string_from_caller(&mut caller, search_ptr).unwrap_or_default();
            let replace = read_string_from_caller(&mut caller, replace_ptr).unwrap_or_default();

            let result = s.replacen(&search, &replace, 1);
            write_string_to_caller(&mut caller, &result)
        },
    )?;

    // string_split / string.split - Split string by delimiter (returns array pointer)
    // CRITICAL: Use WASM malloc to allocate, not State allocator, to avoid memory overlap
    linker.func_wrap(
        "env",
        "string_split",
        |mut caller: Caller<'_, S>, _str_ptr: i32, _delim_ptr: i32| -> i32 {
            // For now, return empty array
            // Full implementation would create array of strings

            // Use WASM malloc to allocate 4 bytes for empty array (length = 0)
            let ptr = if let Some(malloc) = caller.get_export("malloc").and_then(|e| e.into_func()) {
                match malloc.typed::<i32, i32>(&caller) {
                    Ok(typed_malloc) => {
                        match typed_malloc.call(&mut caller, 4) {
                            Ok(p) if p > 0 => p as usize,
                            _ => return 0,
                        }
                    }
                    Err(_) => return 0,
                }
            } else {
                return 0;
            };

            // Re-acquire memory after malloc call
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };

            // Write zero length for empty array
            let _ = memory.write(&mut caller, ptr, &[0u8; 4]);
            ptr as i32
        },
    )?;

    linker.func_wrap(
        "env",
        "string.split",
        |mut caller: Caller<'_, S>, _str_ptr: i32, _delim_ptr: i32| -> i32 {
            // Same as above - return empty array for now using WASM malloc
            let ptr = if let Some(malloc) = caller.get_export("malloc").and_then(|e| e.into_func()) {
                match malloc.typed::<i32, i32>(&caller) {
                    Ok(typed_malloc) => {
                        match typed_malloc.call(&mut caller, 4) {
                            Ok(p) if p > 0 => p as usize,
                            _ => return 0,
                        }
                    }
                    Err(_) => return 0,
                }
            } else {
                return 0;
            };

            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return 0,
            };

            let _ = memory.write(&mut caller, ptr, &[0u8; 4]);
            ptr as i32
        },
    )?;

    // string_index_of - Find substring index
    linker.func_wrap(
        "env",
        "string_index_of",
        |mut caller: Caller<'_, S>, str_ptr: i32, search_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let search = read_string_from_caller(&mut caller, search_ptr).unwrap_or_default();

            s.find(&search).map(|i| i as i32).unwrap_or(-1)
        },
    )?;

    // string_compare - Compare two strings
    linker.func_wrap(
        "env",
        "string_compare",
        |mut caller: Caller<'_, S>, str1_ptr: i32, str2_ptr: i32| -> i32 {
            let s1 = read_string_from_caller(&mut caller, str1_ptr).unwrap_or_default();
            let s2 = read_string_from_caller(&mut caller, str2_ptr).unwrap_or_default();

            match s1.cmp(&s2) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        },
    )?;

    // =========================================
    // TYPE CONVERSIONS (to string)
    // =========================================

    // int_to_string - Convert integer to string
    linker.func_wrap(
        "env",
        "int_to_string",
        |mut caller: Caller<'_, S>, value: i32| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // integer.toString - Method style
    linker.func_wrap(
        "env",
        "integer.toString",
        |mut caller: Caller<'_, S>, value: i32| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // float_to_string - Convert float to string
    linker.func_wrap(
        "env",
        "float_to_string",
        |mut caller: Caller<'_, S>, value: f64| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // number.toString - Method style
    linker.func_wrap(
        "env",
        "number.toString",
        |mut caller: Caller<'_, S>, value: f64| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // bool_to_string - Convert boolean to string
    linker.func_wrap(
        "env",
        "bool_to_string",
        |mut caller: Caller<'_, S>, value: i32| -> i32 {
            let s = if value != 0 { "true" } else { "false" };
            write_string_to_caller(&mut caller, s)
        },
    )?;

    // boolean.toString - Method style
    linker.func_wrap(
        "env",
        "boolean.toString",
        |mut caller: Caller<'_, S>, value: i32| -> i32 {
            let s = if value != 0 { "true" } else { "false" };
            write_string_to_caller(&mut caller, s)
        },
    )?;

    // =========================================
    // TYPE CONVERSIONS (from string)
    // =========================================

    // string_to_int - Convert string to integer
    linker.func_wrap(
        "env",
        "string_to_int",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            s.trim().parse::<i32>().unwrap_or(0)
        },
    )?;

    // string.toInteger - Method style
    linker.func_wrap(
        "env",
        "string.toInteger",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            s.trim().parse::<i32>().unwrap_or(0)
        },
    )?;

    // string_to_float - Convert string to float
    linker.func_wrap(
        "env",
        "string_to_float",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> f64 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            s.trim().parse::<f64>().unwrap_or(0.0)
        },
    )?;

    // string.toNumber - Method style
    linker.func_wrap(
        "env",
        "string.toNumber",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> f64 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            s.trim().parse::<f64>().unwrap_or(0.0)
        },
    )?;

    // string_to_bool - Convert string to boolean
    linker.func_wrap(
        "env",
        "string_to_bool",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let lower = s.trim().to_lowercase();
            if lower == "true" || lower == "1" || lower == "yes" { 1 } else { 0 }
        },
    )?;

    // string.toBoolean - Method style
    linker.func_wrap(
        "env",
        "string.toBoolean",
        |mut caller: Caller<'_, S>, str_ptr: i32| -> i32 {
            let s = read_string_from_caller(&mut caller, str_ptr).unwrap_or_default();
            let lower = s.trim().to_lowercase();
            if lower == "true" || lower == "1" || lower == "yes" { 1 } else { 0 }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_string_operations() {
        assert_eq!("hello world".trim(), "hello world");
        assert_eq!("  hello  ".trim(), "hello");
        assert_eq!("HELLO".to_lowercase(), "hello");
    }
}
