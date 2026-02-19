//! HTTP Client Host Functions
//!
//! Provides HTTP client operations for WASM modules:
//! - http_get, http_post, http_put, http_patch, http_delete
//! - Various helper functions for headers, JSON, forms, etc.
//!
//! All functions are generic over `WasmStateCore` to work with any runtime.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use crate::HttpBridge;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

// Thread-local HTTP bridge for simple operations
// For more complex scenarios, this could be part of state
thread_local! {
    static HTTP_BRIDGE: Arc<RwLock<HttpBridge>> = Arc::new(RwLock::new(HttpBridge::new()));
}

/// Register all HTTP client functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // HTTP GET
    // =========================================

    linker.func_wrap(
        "env",
        "http_get",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => {
                    error!("http_get: Failed to read URL");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            debug!("http_get: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("get", json!({ "url": url })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, body)
                }
                Err(e) => {
                    error!("http_get: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // =========================================
    // HTTP POST
    // =========================================

    linker.func_wrap(
        "env",
        "http_post",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         body_ptr: i32,
         body_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let body = read_raw_string(&mut caller, body_ptr, body_len).unwrap_or_default();

            debug!("http_post: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("post", json!({ "url": url, "body": body })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_post: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // =========================================
    // HTTP PUT
    // =========================================

    linker.func_wrap(
        "env",
        "http_put",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         body_ptr: i32,
         body_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let body = read_raw_string(&mut caller, body_ptr, body_len).unwrap_or_default();

            debug!("http_put: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("put", json!({ "url": url, "body": body })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // =========================================
    // HTTP PATCH
    // =========================================

    linker.func_wrap(
        "env",
        "http_patch",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         body_ptr: i32,
         body_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let body = read_raw_string(&mut caller, body_ptr, body_len).unwrap_or_default();

            debug!("http_patch: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("patch", json!({ "url": url, "body": body })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // =========================================
    // HTTP DELETE
    // =========================================

    linker.func_wrap(
        "env",
        "http_delete",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            debug!("http_delete: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("delete", json!({ "url": url })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // =========================================
    // HTTP HEAD
    // =========================================

    linker.func_wrap(
        "env",
        "http_head",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            debug!("http_head: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("head", json!({ "url": url })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    // Return headers as JSON
                    let headers = v.get("data")
                        .and_then(|d| d.get("headers"))
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    write_string_to_caller(&mut caller, &headers)
                }
                Err(_) => write_string_to_caller(&mut caller, "{}"),
            }
        },
    )?;

    // =========================================
    // HTTP OPTIONS
    // =========================================

    linker.func_wrap(
        "env",
        "http_options",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            debug!("http_options: url={}", url);

            // For now, return empty - OPTIONS is rarely used
            write_string_to_caller(&mut caller, "")
        },
    )?;

    // =========================================
    // HTTP POST JSON
    // =========================================

    linker.func_wrap(
        "env",
        "http_post_json",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         json_ptr: i32,
         json_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let json_body = read_raw_string(&mut caller, json_ptr, json_len).unwrap_or_default();

            debug!("http_post_json: url={}", url);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("post", json!({
                            "url": url,
                            "body": json_body,
                            "headers": { "Content-Type": "application/json" }
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // =========================================
    // HTTP HELPER STUBS (for compatibility)
    // =========================================

    // http_set_user_agent
    linker.func_wrap(
        "env",
        "http_set_user_agent",
        |_: Caller<'_, S>, _ua_ptr: i32, _ua_len: i32| {
            // No-op for now
        },
    )?;

    // http_set_timeout
    linker.func_wrap(
        "env",
        "http_set_timeout",
        |_: Caller<'_, S>, _timeout_ms: i32| {
            // No-op for now
        },
    )?;

    // http_set_max_redirects
    linker.func_wrap(
        "env",
        "http_set_max_redirects",
        |_: Caller<'_, S>, _max: i32| {
            // No-op for now
        },
    )?;

    // http_enable_cookies
    linker.func_wrap(
        "env",
        "http_enable_cookies",
        |_: Caller<'_, S>, _enable: i32| {
            // No-op for now
        },
    )?;

    // http_get_response_code
    linker.func_wrap(
        "env",
        "http_get_response_code",
        |_: Caller<'_, S>| -> i32 {
            200 // Default success
        },
    )?;

    // http_get_response_headers
    linker.func_wrap(
        "env",
        "http_get_response_headers",
        |_: Caller<'_, S>| -> i32 {
            0 // Return null pointer
        },
    )?;

    // http_encode_url - URL-encode a string
    linker.func_wrap(
        "env",
        "http_encode_url",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
            write_string_to_caller(&mut caller, &encoded)
        },
    )?;

    // http_decode_url - URL-decode a string
    linker.func_wrap(
        "env",
        "http_decode_url",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let decoded = url::form_urlencoded::parse(url.as_bytes())
                .map(|(k, v)| if v.is_empty() { k.to_string() } else { format!("{}={}", k, v) })
                .collect::<Vec<_>>()
                .join("&");
            write_string_to_caller(&mut caller, &decoded)
        },
    )?;

    // http_build_query - Build query string from JSON params
    linker.func_wrap(
        "env",
        "http_build_query",
        |mut caller: Caller<'_, S>, params_ptr: i32, params_len: i32| -> i32 {
            let params_json = match read_raw_string(&mut caller, params_ptr, params_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let params: serde_json::Value =
                serde_json::from_str(&params_json).unwrap_or_default();

            let query = if let Some(obj) = params.as_object() {
                let pairs: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| {
                        let val = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        format!(
                            "{}={}",
                            url::form_urlencoded::byte_serialize(k.as_bytes())
                                .collect::<String>(),
                            url::form_urlencoded::byte_serialize(val.as_bytes())
                                .collect::<String>()
                        )
                    })
                    .collect();
                pairs.join("&")
            } else {
                String::new()
            };

            write_string_to_caller(&mut caller, &query)
        },
    )?;

    // http_get_with_headers - GET request with custom headers
    linker.func_wrap(
        "env",
        "http_get_with_headers",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         headers_ptr: i32,
         headers_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let headers_json = read_raw_string(&mut caller, headers_ptr, headers_len)
                .unwrap_or_else(|| "{}".to_string());

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("get", json!({ "url": url, "headers": serde_json::from_str::<serde_json::Value>(&headers_json).unwrap_or_default() })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // http_post_with_headers - POST request with custom headers
    linker.func_wrap(
        "env",
        "http_post_with_headers",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         body_ptr: i32,
         body_len: i32,
         headers_ptr: i32,
         headers_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let body = read_raw_string(&mut caller, body_ptr, body_len).unwrap_or_default();
            let headers_json = read_raw_string(&mut caller, headers_ptr, headers_len)
                .unwrap_or_else(|| "{}".to_string());

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("post", json!({ "url": url, "body": body, "headers": serde_json::from_str::<serde_json::Value>(&headers_json).unwrap_or_default() })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // http_put_json
    linker.func_wrap(
        "env",
        "http_put_json",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         json_ptr: i32,
         json_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let json_body = read_raw_string(&mut caller, json_ptr, json_len).unwrap_or_default();

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("put", json!({
                            "url": url,
                            "body": json_body,
                            "headers": { "Content-Type": "application/json" }
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // http_patch_json
    linker.func_wrap(
        "env",
        "http_patch_json",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         json_ptr: i32,
         json_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let json_body = read_raw_string(&mut caller, json_ptr, json_len).unwrap_or_default();

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("patch", json!({
                            "url": url,
                            "body": json_body,
                            "headers": { "Content-Type": "application/json" }
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    // http_post_form
    linker.func_wrap(
        "env",
        "http_post_form",
        |mut caller: Caller<'_, S>,
         url_ptr: i32,
         url_len: i32,
         form_ptr: i32,
         form_len: i32|
         -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let form_body = read_raw_string(&mut caller, form_ptr, form_len).unwrap_or_default();

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("post", json!({
                            "url": url,
                            "body": form_body,
                            "headers": { "Content-Type": "application/x-www-form-urlencoded" }
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(_) => write_string_to_caller(&mut caller, ""),
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // HTTP client tests would require mocking or real network
}
