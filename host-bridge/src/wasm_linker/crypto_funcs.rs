//! Crypto Host Functions
//!
//! Provides cryptographic operations for WASM modules:
//! - _auth_hash_password: Hash a password using bcrypt
//! - _auth_verify_password: Verify a password against a hash
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use crate::CryptoBridge;
use serde_json::json;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Register all crypto functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // PASSWORD HASHING
    // =========================================

    // _auth_hash_password - Hash a password
    // Args: password_ptr, password_len
    // Returns: pointer to hash string
    linker.func_wrap(
        "env",
        "_auth_hash_password",
        |mut caller: Caller<'_, S>, password_ptr: i32, password_len: i32| -> i32 {
            let password = match read_raw_string(&mut caller, password_ptr, password_len) {
                Some(s) => s,
                None => {
                    error!("_auth_hash_password: Failed to read password");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            debug!("_auth_hash_password: hashing password");

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto.call("hash", json!({ "data": password })).await
                })
            });

            match result {
                Ok(v) => {
                    if let Some(hash) = v.get("data").and_then(|d| d.get("hash")).and_then(|h| h.as_str()) {
                        write_string_to_caller(&mut caller, hash)
                    } else {
                        error!("_auth_hash_password: Invalid hash response");
                        write_string_to_caller(&mut caller, "")
                    }
                }
                Err(e) => {
                    error!("_auth_hash_password: Hash failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // _auth_verify_password - Verify a password against a hash
    // Args: password_ptr, password_len, hash_ptr, hash_len
    // Returns: 1 if valid, 0 if invalid
    linker.func_wrap(
        "env",
        "_auth_verify_password",
        |mut caller: Caller<'_, S>,
         password_ptr: i32,
         password_len: i32,
         hash_ptr: i32,
         hash_len: i32|
         -> i32 {
            let password = match read_raw_string(&mut caller, password_ptr, password_len) {
                Some(s) => s,
                None => {
                    error!("_auth_verify_password: Failed to read password");
                    return 0;
                }
            };

            let hash = match read_raw_string(&mut caller, hash_ptr, hash_len) {
                Some(s) => s,
                None => {
                    error!("_auth_verify_password: Failed to read hash");
                    return 0;
                }
            };

            debug!("_auth_verify_password: verifying password");

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto.call("verify", json!({ "data": password, "hash": hash })).await
                })
            });

            match result {
                Ok(v) => {
                    if v.get("data").and_then(|d| d.get("valid")).and_then(|v| v.as_bool()).unwrap_or(false) {
                        1
                    } else {
                        0
                    }
                }
                Err(e) => {
                    error!("_auth_verify_password: Verify failed: {}", e);
                    0
                }
            }
        },
    )?;

    // =========================================
    // ADDITIONAL CRYPTO UTILITIES
    // =========================================

    // crypto_random_bytes - Generate random bytes
    // Args: len
    // Returns: pointer to random bytes (as hex string)
    linker.func_wrap(
        "env",
        "crypto_random_bytes",
        |mut caller: Caller<'_, S>, len: i32| -> i32 {
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto.call("random", json!({ "length": len })).await
                })
            });

            match result {
                Ok(v) => {
                    if let Some(hex) = v.get("data").and_then(|d| d.get("hex")).and_then(|h| h.as_str()) {
                        write_string_to_caller(&mut caller, hex)
                    } else {
                        write_string_to_caller(&mut caller, "")
                    }
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // crypto_sha256 - Compute SHA256 hash
    // Args: data_ptr, data_len
    // Returns: pointer to hash (as hex string)
    linker.func_wrap(
        "env",
        "crypto_sha256",
        |mut caller: Caller<'_, S>, data_ptr: i32, data_len: i32| -> i32 {
            let data = match read_raw_string(&mut caller, data_ptr, data_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto.call("sha256", json!({ "data": data })).await
                })
            });

            match result {
                Ok(v) => {
                    if let Some(hash) = v.get("data").and_then(|d| d.get("hash")).and_then(|h| h.as_str()) {
                        write_string_to_caller(&mut caller, hash)
                    } else {
                        write_string_to_caller(&mut caller, "")
                    }
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Crypto tests
}
