//! Crypto & JWT Host Functions (Layer 2)
//!
//! Provides cryptographic operations for WASM modules:
//! - _crypto_hash_password: Hash password (bcrypt/argon2)
//! - _crypto_verify_password: Verify password against hash
//! - _crypto_random_bytes: Generate random bytes (base64)
//! - _crypto_random_hex: Generate random hex string
//! - _crypto_hash_sha256: SHA-256 hash
//! - _crypto_hash_sha512: SHA-512 hash
//! - _crypto_hmac: HMAC digest
//! - _jwt_sign: Sign JWT token
//! - _jwt_verify: Verify JWT token
//! - _jwt_decode: Decode JWT without verification
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use crate::CryptoBridge;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde_json::json;
use sha2::{Digest, Sha256, Sha512};
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Register all crypto and JWT functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // PASSWORD HASHING
    // =========================================

    // _crypto_hash_password - Hash a password using bcrypt
    // Args: password_ptr, password_len
    // Returns: pointer to hash string (length-prefixed)
    linker.func_wrap(
        "env",
        "_crypto_hash_password",
        |mut caller: Caller<'_, S>, password_ptr: i32, password_len: i32| -> i32 {
            let password = match read_raw_string(&mut caller, password_ptr, password_len) {
                Some(s) => s,
                None => {
                    error!("_crypto_hash_password: Failed to read password");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            debug!("_crypto_hash_password: hashing password");

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto
                        .call(
                            "hash",
                            json!({
                                "password": password,
                                "algorithm": "bcrypt"
                            }),
                        )
                        .await
                })
            });

            match result {
                Ok(v) if v["ok"] == true => {
                    let hash = v["data"].as_str().unwrap_or("");
                    write_string_to_caller(&mut caller, hash)
                }
                Ok(v) => {
                    error!(
                        "_crypto_hash_password: {}",
                        v["err"]["message"].as_str().unwrap_or("unknown error")
                    );
                    write_string_to_caller(&mut caller, "")
                }
                Err(e) => {
                    error!("_crypto_hash_password: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // _crypto_verify_password - Verify a password against a hash
    // Args: password_ptr, password_len, hash_ptr, hash_len
    // Returns: 1 if valid, 0 if invalid
    linker.func_wrap(
        "env",
        "_crypto_verify_password",
        |mut caller: Caller<'_, S>,
         password_ptr: i32,
         password_len: i32,
         hash_ptr: i32,
         hash_len: i32|
         -> i32 {
            let password = match read_raw_string(&mut caller, password_ptr, password_len) {
                Some(s) => s,
                None => {
                    error!("_crypto_verify_password: Failed to read password");
                    return 0;
                }
            };

            let hash = match read_raw_string(&mut caller, hash_ptr, hash_len) {
                Some(s) => s,
                None => {
                    error!("_crypto_verify_password: Failed to read hash");
                    return 0;
                }
            };

            debug!("_crypto_verify_password: verifying password");

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto
                        .call(
                            "verify",
                            json!({
                                "password": password,
                                "hash": hash
                            }),
                        )
                        .await
                })
            });

            match result {
                Ok(v) if v["ok"] == true => {
                    if v["data"].as_bool().unwrap_or(false) {
                        1
                    } else {
                        0
                    }
                }
                Ok(v) => {
                    error!(
                        "_crypto_verify_password: {}",
                        v["err"]["message"].as_str().unwrap_or("unknown error")
                    );
                    0
                }
                Err(e) => {
                    error!("_crypto_verify_password: {}", e);
                    0
                }
            }
        },
    )?;

    // =========================================
    // RANDOM GENERATION
    // =========================================

    // _crypto_random_bytes - Generate random bytes (base64 encoded)
    // Args: len (number of bytes)
    // Returns: pointer to base64 string (length-prefixed)
    linker.func_wrap(
        "env",
        "_crypto_random_bytes",
        |mut caller: Caller<'_, S>, len: i32| -> i32 {
            if len <= 0 || len > 1_048_576 {
                error!("_crypto_random_bytes: invalid length {}", len);
                return write_string_to_caller(&mut caller, "");
            }
            let mut bytes = vec![0u8; len as usize];
            if rand::rngs::OsRng.try_fill_bytes(&mut bytes).is_err() {
                error!("_crypto_random_bytes: failed to generate random bytes");
                return write_string_to_caller(&mut caller, "");
            }
            let encoded = BASE64.encode(&bytes);
            write_string_to_caller(&mut caller, &encoded)
        },
    )?;

    // _crypto_random_hex - Generate random hex string
    // Args: len (number of bytes, hex output will be 2x this length)
    // Returns: pointer to hex string (length-prefixed)
    linker.func_wrap(
        "env",
        "_crypto_random_hex",
        |mut caller: Caller<'_, S>, len: i32| -> i32 {
            if len <= 0 || len > 1_048_576 {
                error!("_crypto_random_hex: invalid length {}", len);
                return write_string_to_caller(&mut caller, "");
            }
            let mut bytes = vec![0u8; len as usize];
            if rand::rngs::OsRng.try_fill_bytes(&mut bytes).is_err() {
                error!("_crypto_random_hex: failed to generate random bytes");
                return write_string_to_caller(&mut caller, "");
            }
            write_string_to_caller(&mut caller, &hex::encode(&bytes))
        },
    )?;

    // =========================================
    // HASHING
    // =========================================

    // _crypto_hash_sha256 - Compute SHA-256 hash
    // Args: data_ptr, data_len
    // Returns: pointer to hex hash string (length-prefixed)
    linker.func_wrap(
        "env",
        "_crypto_hash_sha256",
        |mut caller: Caller<'_, S>, data_ptr: i32, data_len: i32| -> i32 {
            let data = match read_raw_string(&mut caller, data_ptr, data_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let hash = Sha256::digest(data.as_bytes());
            write_string_to_caller(&mut caller, &hex::encode(hash))
        },
    )?;

    // _crypto_hash_sha512 - Compute SHA-512 hash
    // Args: data_ptr, data_len
    // Returns: pointer to hex hash string (length-prefixed)
    linker.func_wrap(
        "env",
        "_crypto_hash_sha512",
        |mut caller: Caller<'_, S>, data_ptr: i32, data_len: i32| -> i32 {
            let data = match read_raw_string(&mut caller, data_ptr, data_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let hash = Sha512::digest(data.as_bytes());
            write_string_to_caller(&mut caller, &hex::encode(hash))
        },
    )?;

    // _crypto_hmac - Compute HMAC digest
    // Args: data_ptr, data_len, key_ptr, key_len, algo_ptr, algo_len
    // Returns: pointer to hex digest string (length-prefixed)
    linker.func_wrap(
        "env",
        "_crypto_hmac",
        |mut caller: Caller<'_, S>,
         data_ptr: i32,
         data_len: i32,
         key_ptr: i32,
         key_len: i32,
         algo_ptr: i32,
         algo_len: i32|
         -> i32 {
            let data = match read_raw_string(&mut caller, data_ptr, data_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let key = match read_raw_string(&mut caller, key_ptr, key_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let algorithm = read_raw_string(&mut caller, algo_ptr, algo_len)
                .unwrap_or_else(|| "sha256".to_string());

            let result = match algorithm.to_lowercase().as_str() {
                "sha256" => {
                    type HmacSha256 = Hmac<Sha256>;
                    match HmacSha256::new_from_slice(key.as_bytes()) {
                        Ok(mut mac) => {
                            mac.update(data.as_bytes());
                            hex::encode(mac.finalize().into_bytes())
                        }
                        Err(_) => String::new(),
                    }
                }
                "sha512" => {
                    type HmacSha512 = Hmac<Sha512>;
                    match HmacSha512::new_from_slice(key.as_bytes()) {
                        Ok(mut mac) => {
                            mac.update(data.as_bytes());
                            hex::encode(mac.finalize().into_bytes())
                        }
                        Err(_) => String::new(),
                    }
                }
                _ => {
                    error!("_crypto_hmac: unsupported algorithm '{}'", algorithm);
                    String::new()
                }
            };

            write_string_to_caller(&mut caller, &result)
        },
    )?;

    // =========================================
    // JWT
    // =========================================

    // _jwt_sign - Sign a JWT token
    // Args: payload_ptr, payload_len, secret_ptr, secret_len, algo_ptr, algo_len
    // Returns: pointer to JWT token string (length-prefixed)
    linker.func_wrap(
        "env",
        "_jwt_sign",
        |mut caller: Caller<'_, S>,
         payload_ptr: i32,
         payload_len: i32,
         secret_ptr: i32,
         secret_len: i32,
         algo_ptr: i32,
         algo_len: i32|
         -> i32 {
            let payload_str = match read_raw_string(&mut caller, payload_ptr, payload_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let secret = match read_raw_string(&mut caller, secret_ptr, secret_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let algorithm = read_raw_string(&mut caller, algo_ptr, algo_len)
                .unwrap_or_else(|| "HS256".to_string());

            // Parse payload as JSON object
            let payload: serde_json::Value = match serde_json::from_str::<serde_json::Value>(&payload_str) {
                Ok(v) if v.is_object() => v,
                _ => {
                    error!("_jwt_sign: payload must be a JSON object");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto
                        .call(
                            "sign",
                            json!({
                                "payload": payload,
                                "secret": secret,
                                "algorithm": algorithm
                            }),
                        )
                        .await
                })
            });

            match result {
                Ok(v) if v["ok"] == true => {
                    let token = v["data"].as_str().unwrap_or("");
                    write_string_to_caller(&mut caller, token)
                }
                _ => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // _jwt_verify - Verify a JWT token and return payload
    // Args: token_ptr, token_len, secret_ptr, secret_len, algo_ptr, algo_len
    // Returns: pointer to payload JSON string (length-prefixed), empty on failure
    linker.func_wrap(
        "env",
        "_jwt_verify",
        |mut caller: Caller<'_, S>,
         token_ptr: i32,
         token_len: i32,
         secret_ptr: i32,
         secret_len: i32,
         _algo_ptr: i32,
         _algo_len: i32|
         -> i32 {
            let token = match read_raw_string(&mut caller, token_ptr, token_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let secret = match read_raw_string(&mut caller, secret_ptr, secret_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            // Algorithm param is accepted but not used — CryptoBridge tries all HS algorithms
            // for safety (prevents algorithm confusion attacks)

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto
                        .call(
                            "verify_jwt",
                            json!({
                                "token": token,
                                "secret": secret
                            }),
                        )
                        .await
                })
            });

            match result {
                Ok(v) if v["ok"] == true => {
                    let payload = serde_json::to_string(&v["data"]).unwrap_or_default();
                    write_string_to_caller(&mut caller, &payload)
                }
                _ => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // _jwt_decode - Decode JWT without verification (for debugging)
    // Args: token_ptr, token_len
    // Returns: pointer to JSON string with header+payload (length-prefixed)
    linker.func_wrap(
        "env",
        "_jwt_decode",
        |mut caller: Caller<'_, S>, token_ptr: i32, token_len: i32| -> i32 {
            let token = match read_raw_string(&mut caller, token_ptr, token_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut crypto = CryptoBridge::new();
                    crypto
                        .call("decode_jwt", json!({ "token": token }))
                        .await
                })
            });

            match result {
                Ok(v) if v["ok"] == true => {
                    let decoded = serde_json::to_string(&v["data"]).unwrap_or_default();
                    write_string_to_caller(&mut caller, &decoded)
                }
                _ => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Crypto tests are covered by the spec compliance test in mod.rs
    // and by crypto.rs unit tests
}
