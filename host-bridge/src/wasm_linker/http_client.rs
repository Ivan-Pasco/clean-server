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
use std::cell::RefCell;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error};
use wasmtime::{Caller, Linker};

/// Per-thread HTTP client configuration set by http_set_* functions
#[allow(dead_code)]
struct HttpClientConfig {
    timeout_ms: u64,
    user_agent: Option<String>,
    max_redirects: usize,
    cookies_enabled: bool,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 30000,
            user_agent: None,
            max_redirects: 10,
            cookies_enabled: false,
        }
    }
}

/// Captured info from the last HTTP response
struct HttpLastResponse {
    status_code: i32,
    headers_json: String,
}

impl Default for HttpLastResponse {
    fn default() -> Self {
        Self {
            status_code: 0,
            headers_json: "{}".to_string(),
        }
    }
}

// Thread-local HTTP bridge for simple operations
// For more complex scenarios, this could be part of state
thread_local! {
    static HTTP_BRIDGE: Arc<RwLock<HttpBridge>> = Arc::new(RwLock::new(HttpBridge::new()));
    static HTTP_CONFIG: RefCell<HttpClientConfig> = RefCell::new(HttpClientConfig::default());
    static HTTP_LAST_RESPONSE: RefCell<HttpLastResponse> = RefCell::new(HttpLastResponse::default());
}

/// Build request headers JSON including user_agent from config
fn build_request_headers(extra_headers: Option<serde_json::Value>) -> serde_json::Value {
    HTTP_CONFIG.with(|config| {
        let config = config.borrow();
        let mut headers = match extra_headers {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        if let Some(ref ua) = config.user_agent {
            headers.entry("User-Agent").or_insert_with(|| serde_json::Value::String(ua.clone()));
        }
        serde_json::Value::Object(headers)
    })
}

/// Get the configured timeout in ms
fn get_timeout_ms() -> u64 {
    HTTP_CONFIG.with(|config| config.borrow().timeout_ms)
}

/// Get the configured max redirects
fn get_max_redirects() -> usize {
    HTTP_CONFIG.with(|config| config.borrow().max_redirects)
}

/// Store the last HTTP response info from a bridge response
fn store_last_response(result: &serde_json::Value) {
    HTTP_LAST_RESPONSE.with(|last| {
        let mut last = last.borrow_mut();
        if let Some(data) = result.get("data") {
            last.status_code = data.get("status")
                .and_then(|s| s.as_i64())
                .unwrap_or(0) as i32;
            last.headers_json = data.get("headers")
                .map(|h| h.to_string())
                .unwrap_or_else(|| "{}".to_string());
        }
    });
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "GET", "url": url, "headers": headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "POST", "url": url, "body": body, "headers": headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "PUT", "url": url, "body": body, "headers": headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_put: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "PATCH", "url": url, "body": body, "headers": headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_patch: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "DELETE", "url": url, "headers": headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_delete: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let req_headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "HEAD", "url": url, "headers": req_headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    // Return headers as JSON since HEAD responses have no body
                    let resp_headers = v.get("data")
                        .and_then(|d| d.get("headers"))
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    write_string_to_caller(&mut caller, &resp_headers)
                }
                Err(e) => {
                    error!("http_head: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "{}")
                }
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let req_headers = build_request_headers(None);

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "OPTIONS", "url": url, "headers": req_headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_headers = v.get("data")
                        .and_then(|d| d.get("headers"))
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    write_string_to_caller(&mut caller, &resp_headers)
                }
                Err(e) => {
                    error!("http_options: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "{}")
                }
            }
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

            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(Some(json!({ "Content-Type": "application/json" })));

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({
                            "method": "POST",
                            "url": url,
                            "body": json_body,
                            "headers": headers,
                            "timeout": timeout,
                            "max_redirects": max_redirects
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_post_json: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // =========================================
    // HTTP HELPER FUNCTIONS
    // =========================================

    // http_set_user_agent - Store user agent in per-thread config
    linker.func_wrap(
        "env",
        "http_set_user_agent",
        |mut caller: Caller<'_, S>, ua_ptr: i32, ua_len: i32| {
            let ua = read_raw_string(&mut caller, ua_ptr, ua_len).unwrap_or_default();
            debug!("http_set_user_agent: {}", ua);
            HTTP_CONFIG.with(|config| {
                config.borrow_mut().user_agent = if ua.is_empty() { None } else { Some(ua) };
            });
        },
    )?;

    // http_set_timeout - Store timeout in per-thread config
    linker.func_wrap(
        "env",
        "http_set_timeout",
        |_: Caller<'_, S>, timeout_ms: i32| {
            let ms = if timeout_ms <= 0 { 30000 } else { timeout_ms as u64 };
            debug!("http_set_timeout: {}ms", ms);
            HTTP_CONFIG.with(|config| {
                config.borrow_mut().timeout_ms = ms;
            });
        },
    )?;

    // http_set_max_redirects - Store max redirects in per-thread config
    linker.func_wrap(
        "env",
        "http_set_max_redirects",
        |_: Caller<'_, S>, max: i32| {
            let max = if max < 0 { 10 } else { max as usize };
            debug!("http_set_max_redirects: {}", max);
            HTTP_CONFIG.with(|config| {
                config.borrow_mut().max_redirects = max;
            });
        },
    )?;

    // http_enable_cookies - Store cookies flag in per-thread config
    linker.func_wrap(
        "env",
        "http_enable_cookies",
        |_: Caller<'_, S>, enable: i32| {
            let enabled = enable != 0;
            debug!("http_enable_cookies: {}", enabled);
            HTTP_CONFIG.with(|config| {
                config.borrow_mut().cookies_enabled = enabled;
            });
        },
    )?;

    // http_get_response_code - Return status code from the last HTTP response
    linker.func_wrap(
        "env",
        "http_get_response_code",
        |_: Caller<'_, S>| -> i32 {
            HTTP_LAST_RESPONSE.with(|last| last.borrow().status_code)
        },
    )?;

    // http_get_response_headers - Return headers JSON string from the last HTTP response
    linker.func_wrap(
        "env",
        "http_get_response_headers",
        |mut caller: Caller<'_, S>| -> i32 {
            let headers = HTTP_LAST_RESPONSE.with(|last| last.borrow().headers_json.clone());
            write_string_to_caller(&mut caller, &headers)
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

    // http_get_with_headers - GET request with custom headers merged with config
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
            let parsed: serde_json::Value =
                serde_json::from_str(&headers_json).unwrap_or_default();
            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let merged_headers = build_request_headers(Some(parsed));

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "GET", "url": url, "headers": merged_headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, body)
                }
                Err(e) => {
                    error!("http_get_with_headers: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    // http_post_with_headers - POST request with custom headers merged with config
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
            let parsed: serde_json::Value =
                serde_json::from_str(&headers_json).unwrap_or_default();
            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let merged_headers = build_request_headers(Some(parsed));

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({ "method": "POST", "url": url, "body": body, "headers": merged_headers, "timeout": timeout, "max_redirects": max_redirects })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_post_with_headers: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
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
            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(Some(json!({ "Content-Type": "application/json" })));

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({
                            "method": "PUT",
                            "url": url,
                            "body": json_body,
                            "headers": headers,
                            "timeout": timeout,
                            "max_redirects": max_redirects
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_put_json: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
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
            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(Some(json!({ "Content-Type": "application/json" })));

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({
                            "method": "PATCH",
                            "url": url,
                            "body": json_body,
                            "headers": headers,
                            "timeout": timeout,
                            "max_redirects": max_redirects
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_patch_json: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
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
            let timeout = get_timeout_ms();
            let max_redirects = get_max_redirects();
            let headers = build_request_headers(Some(json!({ "Content-Type": "application/x-www-form-urlencoded" })));

            let result = HTTP_BRIDGE.with(|bridge| {
                let bridge = bridge.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let mut b = bridge.write().await;
                        b.call("request", json!({
                            "method": "POST",
                            "url": url,
                            "body": form_body,
                            "headers": headers,
                            "timeout": timeout,
                            "max_redirects": max_redirects
                        })).await
                    })
                })
            });

            match result {
                Ok(v) => {
                    store_last_response(&v);
                    let resp_body = v.get("data")
                        .and_then(|d| d.get("body"))
                        .and_then(|b| b.as_str())
                        .unwrap_or("");
                    write_string_to_caller(&mut caller, resp_body)
                }
                Err(e) => {
                    error!("http_post_form: Request failed: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // HTTP client tests would require mocking or real network
}
