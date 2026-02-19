//! Console I/O Host Functions
//!
//! Provides print, input, and console logging functions for WASM modules.
//! These require host access to stdout/stdin.
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use tracing::info;
use wasmtime::{Caller, Linker};

/// Register all console/IO functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // PRINT FUNCTIONS
    // =========================================

    // print - Print raw bytes without newline
    linker.func_wrap(
        "env",
        "print",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
            if let Some(s) = read_raw_string(&mut caller, ptr, len) {
                print!("{}", s);
            }
        },
    )?;

    // printl - Print raw bytes with newline
    linker.func_wrap(
        "env",
        "printl",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
            if let Some(s) = read_raw_string(&mut caller, ptr, len) {
                println!("{}", s);
            }
        },
    )?;

    // print_string - Print raw string without newline
    linker.func_wrap(
        "env",
        "print_string",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
            if let Some(s) = read_raw_string(&mut caller, ptr, len) {
                print!("{}", s);
            }
        },
    )?;

    // print_integer - Print integer (i64 per spec)
    linker.func_wrap(
        "env",
        "print_integer",
        |_: Caller<'_, S>, value: i64| {
            print!("{}", value);
        },
    )?;

    // print_float - Print float
    linker.func_wrap(
        "env",
        "print_float",
        |_: Caller<'_, S>, value: f64| {
            print!("{}", value);
        },
    )?;

    // print_boolean - Print boolean
    linker.func_wrap(
        "env",
        "print_boolean",
        |_: Caller<'_, S>, value: i32| {
            print!("{}", if value != 0 { "true" } else { "false" });
        },
    )?;

    // =========================================
    // CONSOLE LOGGING FUNCTIONS
    // =========================================

    // console_log - Log message (raw string)
    linker.func_wrap(
        "env",
        "console_log",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
            if let Some(s) = read_raw_string(&mut caller, ptr, len) {
                info!("[LOG] {}", s);
                println!("{}", s);
            }
        },
    )?;

    // console_error - Log error (raw string)
    linker.func_wrap(
        "env",
        "console_error",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
            if let Some(s) = read_raw_string(&mut caller, ptr, len) {
                eprintln!("[ERROR] {}", s);
            }
        },
    )?;

    // console_warn - Log warning (raw string)
    linker.func_wrap(
        "env",
        "console_warn",
        |mut caller: Caller<'_, S>, ptr: i32, len: i32| {
            if let Some(s) = read_raw_string(&mut caller, ptr, len) {
                eprintln!("[WARN] {}", s);
            }
        },
    )?;

    // =========================================
    // INPUT FUNCTIONS
    // =========================================

    // input - Read user input (returns length-prefixed string pointer)
    // Signature: (prompt_ptr: i32, prompt_len: i32) -> i32
    // In server context, returns empty string
    linker.func_wrap(
        "env",
        "input",
        |mut caller: Caller<'_, S>, _prompt_ptr: i32, _prompt_len: i32| -> i32 {
            // In server context, return empty string
            // In CLI context, this would read from stdin
            write_string_to_caller(&mut caller, "")
        },
    )?;

    // console_input - Alias for input
    // Signature: (prompt_ptr: i32, prompt_len: i32) -> i32
    linker.func_wrap(
        "env",
        "console_input",
        |mut caller: Caller<'_, S>, _prompt_ptr: i32, _prompt_len: i32| -> i32 {
            write_string_to_caller(&mut caller, "")
        },
    )?;

    // input_integer - Read integer from user
    // Signature: (prompt_ptr: i32, prompt_len: i32) -> i64
    linker.func_wrap(
        "env",
        "input_integer",
        |_: Caller<'_, S>, _prompt_ptr: i32, _prompt_len: i32| -> i64 {
            // In server context, return 0
            0
        },
    )?;

    // input_float - Read float from user
    // Signature: (prompt_ptr: i32, prompt_len: i32) -> f64
    linker.func_wrap(
        "env",
        "input_float",
        |_: Caller<'_, S>, _prompt_ptr: i32, _prompt_len: i32| -> f64 {
            // In server context, return 0.0
            0.0
        },
    )?;

    // input_yesno - Read yes/no from user
    // Signature: (prompt_ptr: i32, prompt_len: i32) -> i32
    linker.func_wrap(
        "env",
        "input_yesno",
        |_: Caller<'_, S>, _prompt_ptr: i32, _prompt_len: i32| -> i32 {
            // In server context, return false (0)
            0
        },
    )?;

    // input_range - Read integer in range from user
    // Signature: (prompt_ptr: i32, prompt_len: i32, min: i32, max: i32) -> i32
    linker.func_wrap(
        "env",
        "input_range",
        |_: Caller<'_, S>, _prompt_ptr: i32, _prompt_len: i32, min: i32, max: i32| -> i32 {
            // In server context, return min
            let _ = max;
            min
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests would require WASM runtime setup
}
