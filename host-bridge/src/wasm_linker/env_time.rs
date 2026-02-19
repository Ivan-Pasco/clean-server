//! Environment & Time Host Functions (Layer 2)
//!
//! Provides environment and time operations for WASM modules:
//! - _env_get: Get environment variable value
//! - _time_now: Get current Unix timestamp in seconds
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Register environment and time functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // ENVIRONMENT
    // =========================================

    // _env_get - Get environment variable value
    // Args: name_ptr, name_len
    // Returns: pointer to value string (length-prefixed), empty if not found
    linker.func_wrap(
        "env",
        "_env_get",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => {
                    error!("_env_get: Failed to read variable name");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            // Validate variable name (alphanumeric + underscore only)
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            {
                error!("_env_get: Invalid variable name '{}'", name);
                return write_string_to_caller(&mut caller, "");
            }

            // Security: deny access to sensitive variables
            let denied = [
                "AWS_SECRET_ACCESS_KEY",
                "PRIVATE_KEY",
                "ENCRYPTION_KEY",
                "SSH_AUTH_SOCK",
                "GPG_PASSPHRASE",
            ];
            let upper = name.to_uppercase();
            if denied.iter().any(|d| upper.contains(d)) {
                error!("_env_get: Access denied for '{}'", name);
                return write_string_to_caller(&mut caller, "");
            }

            debug!("_env_get: reading '{}'", name);

            match std::env::var(&name) {
                Ok(value) => write_string_to_caller(&mut caller, &value),
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // =========================================
    // TIME
    // =========================================

    // _time_now - Get current Unix timestamp in seconds
    // Args: none
    // Returns: i64 timestamp (seconds since epoch)
    linker.func_wrap(
        "env",
        "_time_now",
        |_caller: Caller<'_, S>| -> i64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        },
    )?;

    Ok(())
}
