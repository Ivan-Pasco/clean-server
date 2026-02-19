//! String Allocation Host Functions
//!
//! Provides string operations that require memory allocation:
//! - concat, substring, trim, toUpper, toLower, replace, split
//! - Type conversions that produce strings: int_to_string, float_to_string, bool_to_string
//! - Type conversions from strings: string_to_int, string_to_float, string_to_bool
//!
//! All string parameters use raw (ptr, len) pairs per HOST_BRIDGE.md spec.
//! Return strings are length-prefixed [4-byte LE length][UTF-8 data].
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use wasmtime::{Caller, Linker};

/// Register all string operation functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // STRING CONCATENATION
    // =========================================

    // string_concat - Concatenate two raw strings
    // Spec: (a_ptr: i32, a_len: i32, b_ptr: i32, b_len: i32) -> i32
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

    // string.concat - Dot notation alias (same signature as string_concat)
    linker.func_wrap(
        "env",
        "string.concat",
        |mut caller: Caller<'_, S>, ptr1: i32, len1: i32, ptr2: i32, len2: i32| -> i32 {
            let s1 = read_raw_string(&mut caller, ptr1, len1).unwrap_or_default();
            let s2 = read_raw_string(&mut caller, ptr2, len2).unwrap_or_default();
            let result = format!("{}{}", s1, s2);
            write_string_to_caller(&mut caller, &result)
        },
    )?;

    // =========================================
    // STRING MANIPULATION
    // =========================================

    // string_substring - Extract substring
    // Spec: (ptr: i32, len: i32, start: i32, end: i32) -> i32
    linker.func_wrap(
        "env",
        "string_substring",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32, start: i32, end: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
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

    // string.substring - Dot notation alias
    linker.func_wrap(
        "env",
        "string.substring",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32, start: i32, end: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
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
    // Spec: (ptr: i32, len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_trim",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.trim",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim())
        },
    )?;

    // string_trim_start - Trim leading whitespace
    linker.func_wrap(
        "env",
        "string_trim_start",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim_start())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.trimStart",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim_start())
        },
    )?;

    // string_trim_end - Trim trailing whitespace
    linker.func_wrap(
        "env",
        "string_trim_end",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim_end())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.trimEnd",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, s.trim_end())
        },
    )?;

    // string_to_upper - Convert to uppercase
    // Spec: (ptr: i32, len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_to_upper",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_uppercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.toUpperCase",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_uppercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string_toUpperCase",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_uppercase())
        },
    )?;

    // string_to_lower - Convert to lowercase
    // Spec: (ptr: i32, len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_to_lower",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_lowercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string.toLowerCase",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_lowercase())
        },
    )?;

    linker.func_wrap(
        "env",
        "string_toLowerCase",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            write_string_to_caller(&mut caller, &s.to_lowercase())
        },
    )?;

    // string_replace - Replace first occurrence
    // Spec: (ptr: i32, len: i32, find_ptr: i32, find_len: i32, replace_ptr: i32, replace_len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_replace",
        |mut caller: Caller<'_, S>,
         ptr: i32, len: i32,
         find_ptr: i32, find_len: i32,
         replace_ptr: i32, replace_len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            let search = read_raw_string(&mut caller, find_ptr, find_len).unwrap_or_default();
            let replace = read_raw_string(&mut caller, replace_ptr, replace_len).unwrap_or_default();

            let result = s.replacen(&search, &replace, 1);
            write_string_to_caller(&mut caller, &result)
        },
    )?;

    linker.func_wrap(
        "env",
        "string.replace",
        |mut caller: Caller<'_, S>,
         ptr: i32, len: i32,
         find_ptr: i32, find_len: i32,
         replace_ptr: i32, replace_len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            let search = read_raw_string(&mut caller, find_ptr, find_len).unwrap_or_default();
            let replace = read_raw_string(&mut caller, replace_ptr, replace_len).unwrap_or_default();

            let result = s.replacen(&search, &replace, 1);
            write_string_to_caller(&mut caller, &result)
        },
    )?;

    // string_split - Split string by delimiter (returns JSON array as length-prefixed string)
    // Spec: (ptr: i32, len: i32, delim_ptr: i32, delim_len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_split",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32, delim_ptr: i32, delim_len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            let delim = read_raw_string(&mut caller, delim_ptr, delim_len).unwrap_or_default();

            let parts: Vec<&str> = s.split(&delim).collect();
            let json = serde_json::to_string(&parts).unwrap_or_else(|_| "[]".to_string());
            write_string_to_caller(&mut caller, &json)
        },
    )?;

    linker.func_wrap(
        "env",
        "string.split",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32, delim_ptr: i32, delim_len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            let delim = read_raw_string(&mut caller, delim_ptr, delim_len).unwrap_or_default();

            let parts: Vec<&str> = s.split(&delim).collect();
            let json = serde_json::to_string(&parts).unwrap_or_else(|_| "[]".to_string());
            write_string_to_caller(&mut caller, &json)
        },
    )?;

    // string_index_of - Find substring index
    // Spec: (haystack_ptr: i32, haystack_len: i32, needle_ptr: i32, needle_len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_index_of",
        |mut caller: Caller<'_, S>,
         haystack_ptr: i32, haystack_len: i32,
         needle_ptr: i32, needle_len: i32| -> i32 {
            let s = read_raw_string(&mut caller, haystack_ptr, haystack_len).unwrap_or_default();
            let search = read_raw_string(&mut caller, needle_ptr, needle_len).unwrap_or_default();

            s.find(&search).map(|i| i as i32).unwrap_or(-1)
        },
    )?;

    // string_compare - Compare two strings lexicographically
    // Spec: (a_ptr: i32, a_len: i32, b_ptr: i32, b_len: i32) -> i32
    // Returns -1 if s1 < s2, 0 if s1 == s2, 1 if s1 > s2
    linker.func_wrap(
        "env",
        "string_compare",
        |mut caller: Caller<'_, S>,
         a_ptr: i32, a_len: i32,
         b_ptr: i32, b_len: i32| -> i32 {
            let s1 = read_raw_string(&mut caller, a_ptr, a_len).unwrap_or_default();
            let s2 = read_raw_string(&mut caller, b_ptr, b_len).unwrap_or_default();

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
    // Spec: (value: i64) -> i32
    linker.func_wrap(
        "env",
        "int_to_string",
        |mut caller: Caller<'_, S>, value: i64| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // integer.toString - Method style alias
    linker.func_wrap(
        "env",
        "integer.toString",
        |mut caller: Caller<'_, S>, value: i64| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // float_to_string - Convert float to string
    // Spec: (value: f64) -> i32
    linker.func_wrap(
        "env",
        "float_to_string",
        |mut caller: Caller<'_, S>, value: f64| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // number.toString - Method style alias
    linker.func_wrap(
        "env",
        "number.toString",
        |mut caller: Caller<'_, S>, value: f64| -> i32 {
            let s = value.to_string();
            write_string_to_caller(&mut caller, &s)
        },
    )?;

    // bool_to_string - Convert boolean to string
    // Spec: (value: i32) -> i32
    linker.func_wrap(
        "env",
        "bool_to_string",
        |mut caller: Caller<'_, S>, value: i32| -> i32 {
            let s = if value != 0 { "true" } else { "false" };
            write_string_to_caller(&mut caller, s)
        },
    )?;

    // boolean.toString - Method style alias
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
    // Spec: (ptr: i32, len: i32) -> i64
    linker.func_wrap(
        "env",
        "string_to_int",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i64 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            s.trim().parse::<i64>().unwrap_or(0)
        },
    )?;

    // string.toInteger - Method style alias
    linker.func_wrap(
        "env",
        "string.toInteger",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i64 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            s.trim().parse::<i64>().unwrap_or(0)
        },
    )?;

    // string_to_float - Convert string to float
    // Spec: (ptr: i32, len: i32) -> f64
    linker.func_wrap(
        "env",
        "string_to_float",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> f64 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            s.trim().parse::<f64>().unwrap_or(0.0)
        },
    )?;

    // string.toNumber - Method style alias
    linker.func_wrap(
        "env",
        "string.toNumber",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> f64 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            s.trim().parse::<f64>().unwrap_or(0.0)
        },
    )?;

    // string_to_bool - Convert string to boolean
    // Spec: (ptr: i32, len: i32) -> i32
    linker.func_wrap(
        "env",
        "string_to_bool",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
            let lower = s.trim().to_lowercase();
            if lower == "true" || lower == "1" || lower == "yes" { 1 } else { 0 }
        },
    )?;

    // string.toBoolean - Method style alias
    linker.func_wrap(
        "env",
        "string.toBoolean",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| -> i32 {
            let s = read_raw_string(&mut caller, ptr, len).unwrap_or_default();
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
