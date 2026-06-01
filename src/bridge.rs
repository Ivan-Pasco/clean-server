//! Host Bridge WASM Imports for Clean Server
//!
//! This module provides server-specific host functions that extend host-bridge:
//!
//! ## Shared Functions (from host-bridge)
//! - Console I/O (print, input)
//! - Math functions (sin, cos, tan, pow, etc.)
//! - String operations (concat, substring, etc.)
//! - Memory runtime (mem_alloc, etc.)
//! - Database (_db_query, _db_execute)
//! - File I/O (file_read, file_write)
//! - HTTP client (http_get, http_post)
//! - Crypto (password hashing)
//!
//! ## Server-Specific Functions (defined here)
//! - HTTP server (_http_listen, _http_route, _http_route_protected, _http_serve_static)
//! - Request context (_req_param, _req_query, _req_body, _req_header, _req_method, _req_path, _req_cookie)
//! - Response manipulation (_res_set_header, _res_redirect)
//! - Session management (_session_store, _session_get, _session_delete, _session_exists, _session_set_csrf, _session_get_csrf, _http_set_cookie)
//! - Session auth (_auth_get_session, _auth_require_auth, _auth_require_role, _auth_can, _auth_has_any_role)
//! - Roles (_roles_register, _role_has_permission, _role_get_permissions)
//! - UI templates (_ui_load_layout, _ui_load_page, _ui_render_page, _ui_inject_head_css, _ui_inject_head_link)

use crate::error::{RuntimeError, RuntimeResult};
use crate::router::HttpMethod;
use crate::session::parse_cookies;
use crate::wasm::{IslandEntry, McpBridgeState, McpPendingRequest, McpTransport, TestResponse, WasmState};
use host_bridge::{read_string_from_caller, read_raw_string, write_string_to_caller};
use tracing::{debug, error, info};
use wasmtime::{Caller, Engine, Linker};

/// Register a Layer 3 bridge function under its canonical `_namespace_fn` name
/// and automatically under its `namespace.fn` dot-notation alias.
///
/// Use this macro for all new Layer 3 bridge functions instead of calling
/// `.func_wrap(...).map_err(...)` directly.  It prevents silently omitting the
/// dot alias that the compiler (>= 0.30.120) also emits.
///
/// The macro calls `func_wrap` once (consuming the closure), then derives the
/// alias via `linker.alias()` — no second closure needed.
///
/// See `foundation/platform-architecture/HOST_BRIDGE.md § Dual Naming`.
macro_rules! register_bridge_fn {
    ($linker:expr, $name:literal, $func:expr) => {{
        $linker
            .func_wrap("env", $name, $func)
            .map_err(|e| RuntimeError::wasm(format!("Failed to define {}: {}", $name, e)))?;
        let _stripped: &str = $name.trim_start_matches('_');
        if $name.starts_with('_') && !$name.starts_with("__") {
            if let Some(_dot_idx) = _stripped.find('_') {
                let _dot_name = format!(
                    "{}.{}",
                    &_stripped[.._dot_idx],
                    &_stripped[_dot_idx + 1..]
                );
                $linker
                    .alias("env", $name, "env", &_dot_name)
                    .map_err(|e| RuntimeError::wasm(format!(
                        "Failed to alias {} -> {}: {}", $name, _dot_name, e
                    )))?;
            }
        }
    }};
}

/// Create a linker with all host functions
///
/// This registers:
/// 1. All shared functions from host-bridge (console, math, string, db, file, http client, crypto)
/// 2. Server-specific functions (HTTP routing, request context)
pub fn create_linker(engine: &Engine) -> RuntimeResult<Linker<WasmState>> {
    let mut linker = Linker::new(engine);

    // Register all shared functions from host-bridge
    host_bridge::register_all_functions(&mut linker).map_err(|e| {
        RuntimeError::wasm(format!("Failed to register host-bridge functions: {}", e))
    })?;

    // Register server-specific functions
    register_http_server_functions(&mut linker)?;
    register_request_context_functions(&mut linker)?;
    register_session_management_functions(&mut linker)?;
    register_session_auth_functions(&mut linker)?;
    register_roles_functions(&mut linker)?;
    register_response_functions(&mut linker)?;
    register_json_functions(&mut linker)?;
    register_islands_functions(&mut linker)?;
    register_ui_functions(&mut linker)?;
    register_async_functions(&mut linker)?;
    register_mcp_functions(&mut linker)?;
    register_test_functions(&mut linker)?;

    // Register dot-notation aliases (compiler >= 0.30.120 emits both forms).
    // See foundation/platform-architecture/HOST_BRIDGE.md § Dual Naming.
    register_dot_aliases(&mut linker)?;

    Ok(linker)
}

/// Check whether the calling WASM module has permission to invoke the named
/// bridge function.
///
/// Returns `true` when the call is permitted. Returns `false` and emits a
/// `WARN`-level log entry when it is denied. Bridge function closures should
/// early-return their zero/error sentinel when this returns `false`.
#[inline]
fn check_bridge_permission(caller: &Caller<'_, WasmState>, func_name: &str) -> bool {
    caller.data().permission_gate.check(func_name)
}

/// Register HTTP server functions (_http_listen, _http_route, _http_route_protected, _http_serve_static)
fn register_http_server_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _http_listen - Start listening on a port
    linker
        .func_wrap(
            "env",
            "_http_listen",
            |mut caller: Caller<'_, WasmState>, port: i32| -> i32 {
                let state = caller.data_mut();
                state.port = port as u16;
                info!("HTTP server configured to listen on port {}", port);
                0 // Return 0 for success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_listen: {}", e)))?;

    // _http_route - Register a route handler
    // Signature: (method_ptr, method_len, path_ptr, path_len, handler_ptr, handler_len) -> i32
    // Strings use raw ptr+len pairs; handler is the WASM export name (e.g. "__route_handler_0")
    linker
        .func_wrap(
            "env",
            "_http_route",
            |mut caller: Caller<'_, WasmState>,
             method_ptr: i32,
             method_len: i32,
             path_ptr: i32,
             path_len: i32,
             handler_ptr: i32,
             handler_len: i32|
             -> i32 {
                let method_str = read_raw_string(&mut caller, method_ptr, method_len)
                    .unwrap_or_else(|| "GET".to_string());
                let path = read_raw_string(&mut caller, path_ptr, path_len)
                    .unwrap_or_else(|| "/".to_string());
                let handler_name = read_raw_string(&mut caller, handler_ptr, handler_len)
                    .unwrap_or_else(|| "__route_handler_0".to_string());

                debug!(
                    "_http_route: method={}, path={}, handler={}",
                    method_str, path, handler_name
                );

                let method = match HttpMethod::parse(&method_str) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Invalid HTTP method '{}': {}", method_str, e);
                        return -1; // Error
                    }
                };

                let state = caller.data();
                let router = state.router.clone();
                // Not protected, no required role
                if let Err(e) =
                    router.register(method, path.clone(), handler_name, false, None)
                {
                    error!("Failed to register route {} {}: {}", method_str, path, e);
                    return -1; // Error
                }
                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_route: {}", e)))?;

    // _http_route_protected - Register a protected route requiring authentication
    // Signature: (method_ptr, method_len, path_ptr, path_len, handler_ptr, handler_len, role_ptr, role_len) -> i32
    // Strings use raw ptr+len pairs; handler is the WASM export name (e.g. "__route_handler_0")
    linker
        .func_wrap(
            "env",
            "_http_route_protected",
            |mut caller: Caller<'_, WasmState>,
             method_ptr: i32,
             method_len: i32,
             path_ptr: i32,
             path_len: i32,
             handler_ptr: i32,
             handler_len: i32,
             role_ptr: i32,
             role_len: i32|
             -> i32 {
                let method_str = read_raw_string(&mut caller, method_ptr, method_len)
                    .unwrap_or_else(|| "GET".to_string());
                let path = read_raw_string(&mut caller, path_ptr, path_len)
                    .unwrap_or_else(|| "/".to_string());
                let handler_name = read_raw_string(&mut caller, handler_ptr, handler_len)
                    .unwrap_or_else(|| "__route_handler_0".to_string());
                let required_role = read_raw_string(&mut caller, role_ptr, role_len)
                    .filter(|s| !s.is_empty());

                debug!(
                    "_http_route_protected: method={}, path={}, handler={}, role={:?}",
                    method_str, path, handler_name, required_role
                );

                let method = match HttpMethod::parse(&method_str) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Invalid HTTP method '{}': {}", method_str, e);
                        return -1; // Error
                    }
                };

                let state = caller.data();
                let router = state.router.clone();
                // Protected route with optional role requirement
                if let Err(e) = router.register(
                    method,
                    path.clone(),
                    handler_name,
                    true,
                    required_role,
                ) {
                    error!(
                        "Failed to register protected route {} {}: {}",
                        method_str, path, e
                    );
                    return -1; // Error
                }
                0 // Success
            },
        )
        .map_err(|e| {
            RuntimeError::wasm(format!("Failed to define _http_route_protected: {}", e))
        })?;

    // _http_serve_static - Mount filesystem directory as static file server
    linker
        .func_wrap(
            "env",
            "_http_serve_static",
            |mut caller: Caller<'_, WasmState>,
             prefix_ptr: i32,
             prefix_len: i32,
             dir_ptr: i32,
             dir_len: i32|
             -> i32 {
                let prefix =
                    match read_raw_string(&mut caller, prefix_ptr, prefix_len) {
                        Some(s) => s,
                        None => {
                            error!("_http_serve_static: Failed to read prefix");
                            return 0;
                        }
                    };
                let dir =
                    match read_raw_string(&mut caller, dir_ptr, dir_len) {
                        Some(s) => s,
                        None => {
                            error!("_http_serve_static: Failed to read dir");
                            return 0;
                        }
                    };

                debug!("_http_serve_static: prefix={}, dir={}", prefix, dir);

                let static_dirs = caller.data().static_dirs.clone();
                match static_dirs.write() {
                    Ok(mut dirs) => {
                        dirs.push((prefix, dir));
                        1 // success
                    }
                    Err(e) => {
                        error!("_http_serve_static: Failed to acquire lock: {}", e);
                        0
                    }
                }
            },
        )
        .map_err(|e| {
            RuntimeError::wasm(format!("Failed to define _http_serve_static: {}", e))
        })?;

    Ok(())
}

/// Register request context functions (_req_param, _req_query, _req_body, etc.)
fn register_request_context_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _req_param - Get a path parameter by name
    linker
        .func_wrap(
            "env",
            "_req_param",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                let param_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s,
                    None => {
                        error!("_req_param: Failed to read param name");
                        return write_string_to_caller(&mut caller, "");
                    }
                };

                debug!("_req_param: Looking for param '{}'", param_name);

                let value = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| ctx.params.get(&param_name).cloned())
                        .unwrap_or_default()
                };

                debug!("_req_param: Returning value '{}' for param '{}'", value, param_name);
                write_string_to_caller(&mut caller, &value)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_param: {}", e)))?;

    // _req_query - Get a query parameter by name
    linker
        .func_wrap(
            "env",
            "_req_query",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                let query_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };

                let value = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| ctx.query.get(&query_name).cloned())
                        .unwrap_or_default()
                };

                write_string_to_caller(&mut caller, &value)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_query: {}", e)))?;

    // _req_body - Get the request body
    linker
        .func_wrap(
            "env",
            "_req_body",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let body = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .map(|ctx| ctx.body.clone())
                        .unwrap_or_default()
                };

                write_string_to_caller(&mut caller, &body)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_body: {}", e)))?;

    // _req_body_field - Get a field from JSON request body
    linker
        .func_wrap(
            "env",
            "_req_body_field",
            |mut caller: Caller<'_, WasmState>, field_ptr: i32, field_len: i32| -> i32 {
                let field_name = match read_raw_string(&mut caller, field_ptr, field_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };

                let value = {
                    let state = caller.data();
                    if let Some(ctx) = state.request_context.as_ref() {
                        let content_type = ctx.headers.iter()
                            .find(|(k, _)| k.to_lowercase() == "content-type")
                            .map(|(_, v)| v.to_lowercase())
                            .unwrap_or_default();
                        if content_type.contains("application/x-www-form-urlencoded") {
                            use url::form_urlencoded;
                            form_urlencoded::parse(ctx.body.as_bytes())
                                .into_owned()
                                .find(|(k, _)| k == &field_name)
                                .map(|(_, v)| v)
                                .unwrap_or_default()
                        } else {
                            serde_json::from_str::<serde_json::Value>(&ctx.body).ok()
                                .and_then(|json| {
                                    json.get(&field_name).map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        serde_json::Value::Null => String::new(),
                                        other => other.to_string(),
                                    })
                                })
                                .unwrap_or_default()
                        }
                    } else {
                        String::new()
                    }
                };

                debug!("_req_body_field({}): {}", field_name, value);
                write_string_to_caller(&mut caller, &value)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_body_field: {}", e)))?;

    // _req_param_int - Get a path parameter as integer
    linker
        .func_wrap(
            "env",
            "_req_param_int",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                let param_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s,
                    None => return 0,
                };

                let value = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| ctx.params.get(&param_name))
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(0)
                };

                debug!("_req_param_int({}): {}", param_name, value);
                value
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_param_int: {}", e)))?;

    // _req_header - Get a request header by name
    linker
        .func_wrap(
            "env",
            "_req_header",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                let header_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s.to_lowercase(),
                    None => return write_string_to_caller(&mut caller, ""),
                };

                let value = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| {
                            ctx.headers
                                .iter()
                                .find(|(k, _)| k.to_lowercase() == header_name)
                                .map(|(_, v)| v.clone())
                        })
                        .unwrap_or_default()
                };

                write_string_to_caller(&mut caller, &value)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_header: {}", e)))?;

    // _req_method - Get the HTTP method
    linker
        .func_wrap(
            "env",
            "_req_method",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let method = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .map(|ctx| ctx.method.clone())
                        .unwrap_or_else(|| "GET".to_string())
                };

                write_string_to_caller(&mut caller, &method)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_method: {}", e)))?;

    // _req_path - Get the request path
    linker
        .func_wrap(
            "env",
            "_req_path",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let path = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .map(|ctx| ctx.path.clone())
                        .unwrap_or_else(|| "/".to_string())
                };

                write_string_to_caller(&mut caller, &path)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_path: {}", e)))?;

    // _req_cookie - Get a cookie value by name
    linker
        .func_wrap(
            "env",
            "_req_cookie",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                let cookie_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };

                debug!("_req_cookie: Looking for cookie '{}'", cookie_name);

                let value = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| {
                            // Find Cookie header
                            ctx.headers
                                .iter()
                                .find(|(k, _)| k.to_lowercase() == "cookie")
                                .and_then(|(_, cookie_header)| {
                                    let cookies = parse_cookies(cookie_header);
                                    cookies.get(&cookie_name).cloned()
                                })
                        })
                        .unwrap_or_default()
                };

                write_string_to_caller(&mut caller, &value)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_cookie: {}", e)))?;

    // _req_headers - Get all request headers as JSON
    linker
        .func_wrap(
            "env",
            "_req_headers",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let headers_json = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .map(|ctx| {
                            let mut map = serde_json::Map::new();
                            for (key, value) in &ctx.headers {
                                map.insert(key.clone(), serde_json::Value::String(value.clone()));
                            }
                            serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
                        })
                        .unwrap_or_else(|| "{}".to_string())
                };

                debug!("_req_headers: Returning all headers");
                write_string_to_caller(&mut caller, &headers_json)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_headers: {}", e)))?;

    // _req_form - Parse form-urlencoded body as JSON
    linker
        .func_wrap(
            "env",
            "_req_form",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let form_json = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .map(|ctx| {
                            use url::form_urlencoded;
                            let params: std::collections::HashMap<String, String> =
                                form_urlencoded::parse(ctx.body.as_bytes())
                                    .into_owned()
                                    .collect();
                            serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string())
                        })
                        .unwrap_or_else(|| "{}".to_string())
                };

                debug!("_req_form: Parsed form data");
                write_string_to_caller(&mut caller, &form_json)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_form: {}", e)))?;

    // _req_ip - Get client IP address
    linker
        .func_wrap(
            "env",
            "_req_ip",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let ip = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| {
                            // Try X-Forwarded-For first (can be comma-separated list)
                            ctx.headers
                                .iter()
                                .find(|(k, _)| k.to_lowercase() == "x-forwarded-for")
                                .and_then(|(_, v)| v.split(',').next().map(|s| s.trim().to_string()))
                                .or_else(|| {
                                    // Try X-Real-IP
                                    ctx.headers
                                        .iter()
                                        .find(|(k, _)| k.to_lowercase() == "x-real-ip")
                                        .map(|(_, v)| v.clone())
                                })
                        })
                        .unwrap_or_else(|| "unknown".to_string())
                };

                debug!("_req_ip: {}", ip);
                write_string_to_caller(&mut caller, &ip)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_ip: {}", e)))?;

    Ok(())
}

/// Register session management functions (_session_store, _session_get, _session_delete, etc.)
/// These align with the frame.auth plugin API using key-value session storage.
fn register_session_management_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _session_store - Store data in session (key-value storage)
    // Args: session_id_ptr, key_ptr, value_ptr, ttl, flags
    // Returns: 1 on success, 0 on error
    linker
        .func_wrap(
            "env",
            "_session_store",
            |mut caller: Caller<'_, WasmState>,
             session_id_ptr: i32,
             key_ptr: i32,
             value_ptr: i32,
             _ttl: i32,
             _flags: i32|
             -> i32 {
                if !check_bridge_permission(&caller, "_session_store") {
                    return 0;
                }
                let session_id = match read_string_from_caller(&mut caller, session_id_ptr) {
                    Some(s) => s,
                    None => {
                        error!("_session_store: Failed to read session ID");
                        return 0;
                    }
                };
                let key = match read_string_from_caller(&mut caller, key_ptr) {
                    Some(s) => s,
                    None => {
                        error!("_session_store: Failed to read key");
                        return 0;
                    }
                };
                let value = match read_string_from_caller(&mut caller, value_ptr) {
                    Some(s) => s,
                    None => {
                        error!("_session_store: Failed to read value");
                        return 0;
                    }
                };

                debug!("_session_store: id={}, key={}, value_len={}", session_id, key, value.len());

                // Store as key=value under the session ID
                let data = if key.is_empty() {
                    value
                } else {
                    // Build JSON object with key-value pair
                    let session_store = caller.data().session_store.clone();
                    let existing = {
                        let mut store = session_store.write().expect("session store lock poisoned");
                        store.get_raw(&session_id)
                    };

                    let mut obj: serde_json::Value = existing
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_else(|| serde_json::json!({}));

                    if let Some(map) = obj.as_object_mut() {
                        map.insert(key, serde_json::Value::String(value));
                    }
                    serde_json::to_string(&obj).unwrap_or_default()
                };

                let session_store = caller.data().session_store.clone();
                let mut store = session_store.write().expect("session store lock poisoned");
                if store.store_raw(&session_id, &data) { 1 } else { 0 }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_store: {}", e)))?;

    // _session_get - Get current session data (returns stored data or empty string)
    // Args: none (session ID comes from request context)
    // Returns: pointer to data string (length-prefixed)
    linker
        .func_wrap(
            "env",
            "_session_get",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                if !check_bridge_permission(&caller, "_session_get") {
                    return write_string_to_caller(&mut caller, "");
                }
                // Get session ID from auth context or cookie
                let session_id = {
                    let state = caller.data();
                    state
                        .auth_context
                        .as_ref()
                        .and_then(|ctx| ctx.session_id.clone())
                        .or_else(|| {
                            state.request_context.as_ref().and_then(|ctx| {
                                ctx.headers
                                    .iter()
                                    .find(|(k, _)| k.to_lowercase() == "cookie")
                                    .and_then(|(_, cookie_header)| {
                                        let cookies = parse_cookies(cookie_header);
                                        cookies.get("session").cloned()
                                            .or_else(|| cookies.get("sid").cloned())
                                    })
                            })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_session_get: No active session");
                        return write_string_to_caller(&mut caller, "");
                    }
                };

                debug!("_session_get: Looking up session {}", session_id);

                let session_store = caller.data().session_store.clone();
                let data = {
                    let mut store = session_store.write().expect("session store lock poisoned");
                    store.get_raw(&session_id)
                };

                match data {
                    Some(d) => {
                        debug!("_session_get: Found data for {}", session_id);
                        write_string_to_caller(&mut caller, &d)
                    }
                    None => {
                        debug!("_session_get: No data found for {}", session_id);
                        write_string_to_caller(&mut caller, "")
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_get: {}", e)))?;

    // _session_delete - Delete current session data
    // Args: none (session ID comes from request context)
    // Returns: 1 if deleted, 0 if not found
    linker
        .func_wrap(
            "env",
            "_session_delete",
            |caller: Caller<'_, WasmState>| -> i32 {
                if !check_bridge_permission(&caller, "_session_delete") {
                    return 0;
                }
                // Get session ID from auth context or cookie
                let session_id = {
                    let state = caller.data();
                    state
                        .auth_context
                        .as_ref()
                        .and_then(|ctx| ctx.session_id.clone())
                        .or_else(|| {
                            state.request_context.as_ref().and_then(|ctx| {
                                ctx.headers
                                    .iter()
                                    .find(|(k, _)| k.to_lowercase() == "cookie")
                                    .and_then(|(_, cookie_header)| {
                                        let cookies = parse_cookies(cookie_header);
                                        cookies.get("session").cloned()
                                            .or_else(|| cookies.get("sid").cloned())
                                    })
                            })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_session_delete: No active session");
                        return 0;
                    }
                };

                info!("_session_delete: Deleting session {}", session_id);

                let session_store = caller.data().session_store.clone();
                let deleted = {
                    let mut store = session_store.write().expect("session store lock poisoned");
                    store.delete_raw(&session_id)
                };

                if deleted { 1 } else { 0 }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_delete: {}", e)))?;

    // _session_exists - Check if a session exists
    // Args: id_ptr, id_len
    // Returns: 1 if exists, 0 if not
    linker
        .func_wrap(
            "env",
            "_session_exists",
            |mut caller: Caller<'_, WasmState>, id_ptr: i32, id_len: i32| -> i32 {
                if !check_bridge_permission(&caller, "_session_exists") {
                    return 0;
                }
                let session_id = match read_raw_string(&mut caller, id_ptr, id_len) {
                    Some(s) => s,
                    None => return 0,
                };

                let session_store = caller.data().session_store.clone();
                let store = session_store.read().expect("session store lock poisoned");
                if store.exists_raw(&session_id) { 1 } else { 0 }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_exists: {}", e)))?;

    // _session_set_csrf - Store CSRF token for the current session
    // Args: token_ptr, token_len
    // Returns: 1 on success, 0 if no current session
    linker
        .func_wrap(
            "env",
            "_session_set_csrf",
            |mut caller: Caller<'_, WasmState>, token_ptr: i32, token_len: i32| -> i32 {
                let token = match read_raw_string(&mut caller, token_ptr, token_len) {
                    Some(s) => s,
                    None => {
                        error!("_session_set_csrf: Failed to read token");
                        return 0;
                    }
                };

                // Get current session ID from auth context or cookie
                let session_id = {
                    let state = caller.data();
                    state
                        .auth_context
                        .as_ref()
                        .and_then(|ctx| ctx.session_id.clone())
                        .or_else(|| {
                            state.request_context.as_ref().and_then(|ctx| {
                                ctx.headers
                                    .iter()
                                    .find(|(k, _)| k.to_lowercase() == "cookie")
                                    .and_then(|(_, cookie_header)| {
                                        let cookies = parse_cookies(cookie_header);
                                        cookies.get("session").cloned()
                                            .or_else(|| cookies.get("sid").cloned())
                                    })
                            })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_session_set_csrf: No active session");
                        return 0;
                    }
                };

                debug!("_session_set_csrf: Setting CSRF for session {}", session_id);
                let session_store = caller.data().session_store.clone();
                let mut store = session_store.write().expect("session store lock poisoned");
                store.set_csrf(&session_id, &token);
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_set_csrf: {}", e)))?;

    // _session_get_csrf - Get CSRF token for the current session
    // Returns: pointer to CSRF token string (or empty if none)
    linker
        .func_wrap(
            "env",
            "_session_get_csrf",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                // Get current session ID from auth context or cookie
                let session_id = {
                    let state = caller.data();
                    state
                        .auth_context
                        .as_ref()
                        .and_then(|ctx| ctx.session_id.clone())
                        .or_else(|| {
                            state.request_context.as_ref().and_then(|ctx| {
                                ctx.headers
                                    .iter()
                                    .find(|(k, _)| k.to_lowercase() == "cookie")
                                    .and_then(|(_, cookie_header)| {
                                        let cookies = parse_cookies(cookie_header);
                                        cookies.get("session").cloned()
                                            .or_else(|| cookies.get("sid").cloned())
                                    })
                            })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_session_get_csrf: No active session");
                        return write_string_to_caller(&mut caller, "");
                    }
                };

                let session_store = caller.data().session_store.clone();
                let store = session_store.read().expect("session store lock poisoned");
                let token = store.get_csrf(&session_id).unwrap_or_default();
                write_string_to_caller(&mut caller, &token)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_get_csrf: {}", e)))?;

    // _http_set_cookie - Set a cookie with name and value
    // Args: name_ptr, value_ptr (length-prefixed string pointers)
    // Returns: 1 on success, 0 on error
    linker
        .func_wrap(
            "env",
            "_http_set_cookie",
            |mut caller: Caller<'_, WasmState>,
             name_ptr: i32,
             value_ptr: i32|
             -> i32 {
                if !check_bridge_permission(&caller, "_http_set_cookie") {
                    return 0;
                }
                let name = match read_string_from_caller(&mut caller, name_ptr) {
                    Some(s) => s,
                    None => {
                        error!("_http_set_cookie: Failed to read cookie name");
                        return 0;
                    }
                };
                let value = match read_string_from_caller(&mut caller, value_ptr) {
                    Some(s) => s,
                    None => {
                        error!("_http_set_cookie: Failed to read cookie value");
                        return 0;
                    }
                };

                // Build cookie string: name=value with sensible defaults
                let cookie = format!("{}={}; Path=/; HttpOnly", name, value);

                debug!("_http_set_cookie: {}", cookie);
                caller.data_mut().pending_set_cookie = Some(cookie);
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_set_cookie: {}", e)))?;

    Ok(())
}

/// Register role-based permission functions (_roles_register, _role_has_permission, _role_get_permissions)
fn register_roles_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _roles_register - Register role definitions from JSON
    // Args: json_ptr, json_len (JSON object: { "admin": ["read", "write", "delete"], "user": ["read"] })
    // Returns: 1 on success, 0 on error
    linker
        .func_wrap(
            "env",
            "_roles_register",
            |mut caller: Caller<'_, WasmState>, json_ptr: i32, json_len: i32| -> i32 {
                if !check_bridge_permission(&caller, "_roles_register") {
                    return 0;
                }
                let config_json = match read_raw_string(&mut caller, json_ptr, json_len) {
                    Some(s) => s,
                    None => {
                        error!("_roles_register: Failed to read JSON");
                        return 0;
                    }
                };

                debug!("_roles_register: {}", config_json);

                let roles_store = caller.data().roles_store.clone();
                let mut store = roles_store.write().expect("roles store lock poisoned");
                if store.register(&config_json) { 1 } else { 0 }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _roles_register: {}", e)))?;

    // _role_has_permission - Check if a role has a specific permission
    // Args: role_ptr, role_len, perm_ptr, perm_len
    // Returns: 1 if has permission, 0 if not
    linker
        .func_wrap(
            "env",
            "_role_has_permission",
            |mut caller: Caller<'_, WasmState>,
             role_ptr: i32,
             role_len: i32,
             perm_ptr: i32,
             perm_len: i32|
             -> i32 {
                if !check_bridge_permission(&caller, "_role_has_permission") {
                    return 0;
                }
                let role = match read_raw_string(&mut caller, role_ptr, role_len) {
                    Some(s) => s,
                    None => return 0,
                };
                let permission = match read_raw_string(&mut caller, perm_ptr, perm_len) {
                    Some(s) => s,
                    None => return 0,
                };

                let roles_store = caller.data().roles_store.clone();
                let store = roles_store.read().expect("roles store lock poisoned");
                if store.has_permission(&role, &permission) { 1 } else { 0 }
            },
        )
        .map_err(|e| {
            RuntimeError::wasm(format!("Failed to define _role_has_permission: {}", e))
        })?;

    // _role_get_permissions - Get all permissions for a role as JSON array
    // Args: role_ptr, role_len
    // Returns: pointer to JSON array string (e.g., '["read","write"]')
    linker
        .func_wrap(
            "env",
            "_role_get_permissions",
            |mut caller: Caller<'_, WasmState>, role_ptr: i32, role_len: i32| -> i32 {
                if !check_bridge_permission(&caller, "_role_get_permissions") {
                    return write_string_to_caller(&mut caller, "[]");
                }
                let role = match read_raw_string(&mut caller, role_ptr, role_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, "[]"),
                };

                let roles_store = caller.data().roles_store.clone();
                let store = roles_store.read().expect("roles store lock poisoned");
                let permissions = store.get_permissions(&role);
                let json = serde_json::to_string(&permissions).unwrap_or_else(|_| "[]".to_string());

                debug!("_role_get_permissions: role={} -> {}", role, json);
                write_string_to_caller(&mut caller, &json)
            },
        )
        .map_err(|e| {
            RuntimeError::wasm(format!("Failed to define _role_get_permissions: {}", e))
        })?;

    Ok(())
}

/// Register session-based authentication functions
fn register_session_auth_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _auth_get_session - Get session info from current request
    linker
        .func_wrap(
            "env",
            "_auth_get_session",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                if !check_bridge_permission(&caller, "_auth_get_session") {
                    return write_string_to_caller(&mut caller, "null");
                }
                let session_json = {
                    let state = caller.data();
                    if let Some(auth) = &state.auth_context {
                        serde_json::json!({
                            "user_id": auth.user_id,
                            "role": auth.role,
                            "session_id": auth.session_id
                        })
                        .to_string()
                    } else {
                        "null".to_string()
                    }
                };

                write_string_to_caller(&mut caller, &session_json)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_get_session: {}", e)))?;

    // _auth_require_auth - Check if user is authenticated
    linker
        .func_wrap(
            "env",
            "_auth_require_auth",
            |caller: Caller<'_, WasmState>| -> i32 {
                let state = caller.data();
                if state.auth_context.is_some() { 1 } else { 0 }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_require_auth: {}", e)))?;

    // _auth_require_role - Check if user has a specific role
    linker
        .func_wrap(
            "env",
            "_auth_require_role",
            |mut caller: Caller<'_, WasmState>, role_ptr: i32, role_len: i32| -> i32 {
                let required_role = match read_raw_string(&mut caller, role_ptr, role_len) {
                    Some(s) => s,
                    None => return 0,
                };

                let state = caller.data();
                if let Some(auth) = &state.auth_context {
                    if auth.role == required_role { 1 } else { 0 }
                } else {
                    0
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_require_role: {}", e)))?;

    // _auth_can - Check if user has permission (role-based)
    linker
        .func_wrap(
            "env",
            "_auth_can",
            |mut caller: Caller<'_, WasmState>, permission_ptr: i32, permission_len: i32| -> i32 {
                let permission = match read_raw_string(&mut caller, permission_ptr, permission_len) {
                    Some(s) => s,
                    None => return 0,
                };

                let state = caller.data();
                if let Some(auth) = &state.auth_context {
                    // Simple role-based check: admin can do anything
                    if auth.role == "admin" || auth.role == permission {
                        1
                    } else {
                        0
                    }
                } else {
                    0
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_can: {}", e)))?;

    // _auth_has_any_role - Check if user has any of the specified roles
    linker
        .func_wrap(
            "env",
            "_auth_has_any_role",
            |mut caller: Caller<'_, WasmState>, roles_ptr: i32, roles_len: i32| -> i32 {
                let roles_json = match read_raw_string(&mut caller, roles_ptr, roles_len) {
                    Some(s) => s,
                    None => return 0,
                };

                let roles: Vec<String> = serde_json::from_str(&roles_json).unwrap_or_default();

                let state = caller.data();
                if let Some(auth) = &state.auth_context {
                    if roles.contains(&auth.role) { 1 } else { 0 }
                } else {
                    0
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_has_any_role: {}", e)))?;

    // _auth_set_session - Set session data from JSON
    // Args: data_ptr, data_len (JSON with user_id, role, claims)
    // Returns: 1 on success, 0 on error
    linker
        .func_wrap(
            "env",
            "_auth_set_session",
            |mut caller: Caller<'_, WasmState>, data_ptr: i32, data_len: i32| -> i32 {
                if !check_bridge_permission(&caller, "_auth_set_session") {
                    return 0;
                }
                let data_json = match read_raw_string(&mut caller, data_ptr, data_len) {
                    Some(s) => s,
                    None => return 0,
                };

                debug!("_auth_set_session: data={}", data_json);

                let parsed: serde_json::Value = match serde_json::from_str(&data_json) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("_auth_set_session: Invalid JSON: {}", e);
                        return 0;
                    }
                };

                let user_id = parsed
                    .get("user_id")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                let role = parsed
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string();
                let claims = parsed
                    .get("claims")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());

                info!("_auth_set_session: user_id={}, role={}", user_id, role);

                let session_store = caller.data().session_store.clone();

                let session = {
                    let mut store = session_store.write().expect("session store lock poisoned");
                    store.create(user_id, &role, &claims)
                };

                let session_id = session.session_id.clone();

                let set_cookie = {
                    let store = session_store.read().expect("session store lock poisoned");
                    store.format_cookie(&session_id)
                };

                caller.data_mut().pending_set_cookie = Some(set_cookie);
                caller
                    .data_mut()
                    .set_auth_from_session(user_id, role, session_id);

                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_set_session: {}", e)))?;

    // _auth_clear_session - Clear the current session
    // Returns: 1 on success, 0 if no session
    linker
        .func_wrap(
            "env",
            "_auth_clear_session",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                if !check_bridge_permission(&caller, "_auth_clear_session") {
                    return 0;
                }
                let session_id = {
                    let state = caller.data();
                    state
                        .auth_context
                        .as_ref()
                        .and_then(|ctx| ctx.session_id.clone())
                        .or_else(|| {
                            state.request_context.as_ref().and_then(|ctx| {
                                ctx.headers
                                    .iter()
                                    .find(|(k, _)| k.to_lowercase() == "cookie")
                                    .and_then(|(_, cookie_header)| {
                                        let cookies = parse_cookies(cookie_header);
                                        cookies
                                            .get("session")
                                            .cloned()
                                            .or_else(|| cookies.get("sid").cloned())
                                    })
                            })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_auth_clear_session: No session to clear");
                        return 0;
                    }
                };

                info!("_auth_clear_session: Clearing session {}", session_id);

                let session_store = caller.data().session_store.clone();

                {
                    let mut store = session_store.write().expect("session store lock poisoned");
                    store.delete(&session_id);
                }

                let clear_cookie = {
                    let store = session_store.read().expect("session store lock poisoned");
                    store.format_clear_cookie()
                };

                caller.data_mut().pending_set_cookie = Some(clear_cookie);
                caller.data_mut().auth_context = None;

                1
            },
        )
        .map_err(|e| {
            RuntimeError::wasm(format!("Failed to define _auth_clear_session: {}", e))
        })?;

    // _auth_user_id - Get the current user's ID
    linker
        .func_wrap(
            "env",
            "_auth_user_id",
            |caller: Caller<'_, WasmState>| -> i32 {
                let state = caller.data();
                state.auth_context.as_ref().map(|a| a.user_id).unwrap_or(0)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_user_id: {}", e)))?;

    // _auth_user_role - Get the current user's role
    linker
        .func_wrap(
            "env",
            "_auth_user_role",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let role = {
                    let state = caller.data();
                    state
                        .auth_context
                        .as_ref()
                        .map(|a| a.role.clone())
                        .unwrap_or_default()
                };
                write_string_to_caller(&mut caller, &role)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_user_role: {}", e)))?;

    Ok(())
}

/// Register response manipulation functions (_res_set_header, _res_redirect)
fn register_response_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _http_respond - Send an HTTP response with status, content type, and body
    // Args: status, content_type_ptr, content_type_len, body_ptr, body_len
    // Returns: pointer to body (for chaining)
    linker
        .func_wrap(
            "env",
            "_http_respond",
            |mut caller: Caller<'_, WasmState>,
             status: i32,
             content_type_ptr: i32,
             content_type_len: i32,
             body_ptr: i32,
             body_len: i32|
             -> i32 {
                let content_type = read_raw_string(&mut caller, content_type_ptr, content_type_len)
                    .unwrap_or_else(|| "text/plain".to_string());
                let body = read_raw_string(&mut caller, body_ptr, body_len)
                    .unwrap_or_default();

                debug!(
                    "_http_respond: status={}, content_type={}, body_len={}",
                    status,
                    content_type,
                    body.len()
                );

                // Set response properties
                let state = caller.data_mut();
                state.set_status(status as u16);
                state.add_header("Content-Type".to_string(), content_type);
                state.set_body(body.clone());

                // Return pointer to body for chaining
                write_string_to_caller(&mut caller, &body)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_respond: {}", e)))?;

    // _http_redirect - Send an HTTP redirect (alternative signature)
    // Args: status, url_ptr, url_len
    // Returns: pointer to url
    linker
        .func_wrap(
            "env",
            "_http_redirect",
            |mut caller: Caller<'_, WasmState>, status: i32, url_ptr: i32, url_len: i32| -> i32 {
                let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };

                debug!("_http_redirect: status={}, url={}", status, url);

                caller.data_mut().set_redirect(status as u16, url.clone());

                write_string_to_caller(&mut caller, &url)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_redirect: {}", e)))?;

    // _http_set_header - Alias for _res_set_header
    // Args: name_ptr, name_len, value_ptr, value_len
    // Returns: pointer to header name
    linker
        .func_wrap(
            "env",
            "_http_set_header",
            |mut caller: Caller<'_, WasmState>,
             name_ptr: i32,
             name_len: i32,
             value_ptr: i32,
             value_len: i32|
             -> i32 {
                let header_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };
                let header_value = read_raw_string(&mut caller, value_ptr, value_len)
                    .unwrap_or_default();

                debug!("_http_set_header: {}={}", header_name, header_value);
                caller.data_mut().add_header(header_name.clone(), header_value);

                write_string_to_caller(&mut caller, &header_name)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_set_header: {}", e)))?;

    // _res_set_header - Set a custom response header
    // Args: name_ptr, name_len, value_ptr, value_len
    // Returns: 1 on success, 0 on error
    linker
        .func_wrap(
            "env",
            "_res_set_header",
            |mut caller: Caller<'_, WasmState>,
             name_ptr: i32,
             name_len: i32,
             value_ptr: i32,
             value_len: i32|
             -> i32 {
                if !check_bridge_permission(&caller, "_res_set_header") {
                    return 0;
                }
                let header_name = match read_raw_string(&mut caller, name_ptr, name_len) {
                    Some(s) => s,
                    None => {
                        error!("_res_set_header: Failed to read header name");
                        return 0;
                    }
                };

                let header_value = match read_raw_string(&mut caller, value_ptr, value_len) {
                    Some(s) => s,
                    None => {
                        error!("_res_set_header: Failed to read header value");
                        return 0;
                    }
                };

                debug!("_res_set_header: {}={}", header_name, header_value);
                caller.data_mut().add_header(header_name, header_value);
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _res_set_header: {}", e)))?;

    // _res_redirect - Set a redirect response
    // Args: url_ptr, url_len, status_code (301, 302, 307, 308)
    // Returns: 1 on success, 0 on error
    // Note: Status codes:
    //   301 = Moved Permanently (cacheable, may change method to GET)
    //   302 = Found (temporary, may change method to GET)
    //   307 = Temporary Redirect (preserves method)
    //   308 = Permanent Redirect (preserves method)
    linker
        .func_wrap(
            "env",
            "_res_redirect",
            |mut caller: Caller<'_, WasmState>,
             url_ptr: i32,
             url_len: i32,
             status_code: i32|
             -> i32 {
                if !check_bridge_permission(&caller, "_res_redirect") {
                    return 0;
                }
                let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                    Some(s) => s,
                    None => {
                        error!("_res_redirect: Failed to read URL");
                        return 0;
                    }
                };

                // Validate status code
                let status = match status_code {
                    301 | 302 | 303 | 307 | 308 => status_code as u16,
                    _ => {
                        debug!(
                            "_res_redirect: Invalid status code {}, defaulting to 302",
                            status_code
                        );
                        302
                    }
                };

                info!("_res_redirect: {} -> {}", status, url);
                caller.data_mut().set_redirect(status, url);
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _res_redirect: {}", e)))?;

    // _res_status - Set response status code
    // Args: code (i32)
    linker
        .func_wrap(
            "env",
            "_res_status",
            |mut caller: Caller<'_, WasmState>, code: i32| {
                debug!("_res_status: {}", code);
                caller.data_mut().set_status(code as u16);
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _res_status: {}", e)))?;

    // _res_body - Set response body
    // Args: body_ptr, body_len
    linker
        .func_wrap(
            "env",
            "_res_body",
            |mut caller: Caller<'_, WasmState>, body_ptr: i32, body_len: i32| {
                let body = read_raw_string(&mut caller, body_ptr, body_len).unwrap_or_default();
                debug!("_res_body: {} bytes", body.len());
                caller.data_mut().set_body(body);
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _res_body: {}", e)))?;

    // _res_json - Set JSON response (sets body + Content-Type header)
    // Args: json_ptr, json_len
    linker
        .func_wrap(
            "env",
            "_res_json",
            |mut caller: Caller<'_, WasmState>, json_ptr: i32, json_len: i32| {
                let json_body = read_raw_string(&mut caller, json_ptr, json_len).unwrap_or_default();
                debug!("_res_json: {} bytes", json_body.len());
                caller
                    .data_mut()
                    .add_header("Content-Type".to_string(), "application/json".to_string());
                caller.data_mut().set_body(json_body);
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _res_json: {}", e)))?;

    // _http_set_cache - Set Cache-Control max-age header
    linker
        .func_wrap(
            "env",
            "_http_set_cache",
            |mut caller: Caller<'_, WasmState>, max_age: i32| -> i32 {
                let cache_value = if max_age > 0 {
                    format!("public, max-age={}", max_age)
                } else {
                    "no-cache, no-store, must-revalidate".to_string()
                };

                debug!("_http_set_cache: {}", cache_value);
                caller.data_mut().add_header("Cache-Control".to_string(), cache_value);
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_set_cache: {}", e)))?;

    // _http_no_cache - Disable caching completely
    linker
        .func_wrap(
            "env",
            "_http_no_cache",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                debug!("_http_no_cache: Disabling cache");
                caller.data_mut().add_header(
                    "Cache-Control".to_string(),
                    "no-cache, no-store, must-revalidate".to_string(),
                );
                caller.data_mut().add_header("Pragma".to_string(), "no-cache".to_string());
                caller.data_mut().add_header("Expires".to_string(), "0".to_string());
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_no_cache: {}", e)))?;

    Ok(())
}

/// Register JSON encode/decode/query functions (_json_encode, _json_decode, _json_get)
fn register_json_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _json_encode - Serialize value to JSON string
    linker
        .func_wrap(
            "env",
            "_json_encode",
            |mut caller: Caller<'_, WasmState>, value_ptr: i32, value_len: i32| -> i32 {
                let value = match read_raw_string(&mut caller, value_ptr, value_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, "null"),
                };

                // Try to parse as JSON first to validate and re-serialize
                match serde_json::from_str::<serde_json::Value>(&value) {
                    Ok(json_value) => {
                        let encoded = serde_json::to_string(&json_value)
                            .unwrap_or_else(|_| "null".to_string());
                        debug!("_json_encode: encoded {} bytes", encoded.len());
                        write_string_to_caller(&mut caller, &encoded)
                    }
                    Err(_) => {
                        // If not valid JSON, treat as a string value and encode it
                        let json_str = serde_json::Value::String(value);
                        let encoded = serde_json::to_string(&json_str)
                            .unwrap_or_else(|_| "\"\"".to_string());
                        debug!("_json_encode: encoded string as JSON");
                        write_string_to_caller(&mut caller, &encoded)
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _json_encode: {}", e)))?;

    // _json_decode - Parse JSON string to value
    linker
        .func_wrap(
            "env",
            "_json_decode",
            |mut caller: Caller<'_, WasmState>, json_ptr: i32, json_len: i32| -> i32 {
                let json_str = match read_raw_string(&mut caller, json_ptr, json_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, "null"),
                };

                match serde_json::from_str::<serde_json::Value>(&json_str) {
                    Ok(value) => {
                        let decoded = serde_json::to_string(&value)
                            .unwrap_or_else(|_| "null".to_string());
                        debug!("_json_decode: decoded {} bytes", json_str.len());
                        write_string_to_caller(&mut caller, &decoded)
                    }
                    Err(e) => {
                        error!("_json_decode: parse error: {}", e);
                        let error_json = serde_json::json!({
                            "error": "JSON parse error",
                            "message": e.to_string()
                        })
                        .to_string();
                        write_string_to_caller(&mut caller, &error_json)
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _json_decode: {}", e)))?;

    // _json_get - Extract value from JSON by dot-separated path
    linker
        .func_wrap(
            "env",
            "_json_get",
            |mut caller: Caller<'_, WasmState>,
             json_ptr: i32,
             json_len: i32,
             path_ptr: i32,
             path_len: i32|
             -> i32 {
                let json_str = match read_raw_string(&mut caller, json_ptr, json_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };
                let path = match read_raw_string(&mut caller, path_ptr, path_len) {
                    Some(s) => s,
                    None => return write_string_to_caller(&mut caller, ""),
                };

                let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
                    Ok(v) => v,
                    Err(_) => return write_string_to_caller(&mut caller, ""),
                };

                let mut current = &parsed;
                for part in path.split('.') {
                    // Support both numeric array indices (e.g. "rows.0.slug") and object keys
                    let next = if let Ok(idx) = part.parse::<usize>() {
                        current.get(idx)
                    } else {
                        current.get(part)
                    };
                    match next {
                        Some(v) => current = v,
                        None => return write_string_to_caller(&mut caller, ""),
                    }
                }

                let result = match current {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };

                debug!("_json_get: path='{}' -> '{}'", path, result);
                write_string_to_caller(&mut caller, &result)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _json_get: {}", e)))?;

    Ok(())
}

/// Register island component functions (_island_register)
fn register_islands_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _island_register - Register an island component for client-side hydration
    //
    // Parameters:
    //   component_ptr/len - Component name identifier (e.g. "Counter")
    //   module_ptr/len    - URL path to the island's WASM module (e.g. "/islands/counter.wasm")
    //   hydration_ptr/len - Hydration strategy: "on" | "visible" | "idle" | "only"
    //
    // Returns: 0 on success, -1 if the hydration mode is invalid
    linker
        .func_wrap(
            "env",
            "_island_register",
            |mut caller: Caller<'_, WasmState>,
             component_ptr: i32,
             component_len: i32,
             module_ptr: i32,
             module_len: i32,
             hydration_ptr: i32,
             hydration_len: i32|
             -> i32 {
                let component = match read_raw_string(&mut caller, component_ptr, component_len) {
                    Some(s) if !s.is_empty() => s,
                    _ => {
                        error!("_island_register: Failed to read component name or component name is empty");
                        return -1;
                    }
                };

                let module_path = match read_raw_string(&mut caller, module_ptr, module_len) {
                    Some(s) if !s.is_empty() => s,
                    _ => {
                        error!("_island_register: Failed to read module path or module path is empty");
                        return -1;
                    }
                };

                let hydration = match read_raw_string(&mut caller, hydration_ptr, hydration_len) {
                    Some(s) => s,
                    None => {
                        error!("_island_register: Failed to read hydration mode");
                        return -1;
                    }
                };

                // Validate hydration strategy — only recognised modes are permitted
                match hydration.as_str() {
                    "on" | "visible" | "idle" | "only" => {}
                    other => {
                        error!(
                            "_island_register: Invalid hydration mode '{}' for component '{}'. \
                             Must be one of: on, visible, idle, only",
                            other, component
                        );
                        return -1;
                    }
                }

                info!(
                    "_island_register: component='{}', module='{}', hydration='{}'",
                    component, module_path, hydration
                );

                let entry = IslandEntry {
                    component,
                    module: module_path,
                    hydration,
                };

                let islands_store = caller.data().islands_store.clone();
                match islands_store.write() {
                    Ok(mut store) => {
                        store.islands.push(entry);
                        0 // success
                    }
                    Err(e) => {
                        error!("_island_register: Failed to acquire islands store lock: {}", e);
                        -1
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _island_register: {}", e)))?;

    Ok(())
}

/// Register UI template bridge functions (_ui_load_layout, _ui_load_page, _ui_render_page, _ui_inject_head_css, _ui_inject_head_link)
fn register_ui_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _ui_load_layout - Load an HTML layout file. Caller provides the full relative path
    // (e.g. "app/ui/layouts/main.html"). Path resolution is the caller's responsibility.
    register_bridge_fn!(
        linker,
        "_ui_load_layout",
        |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
            let path_str = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    error!("_ui_load_layout: Failed to read path");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            let cwd = std::env::current_dir().unwrap_or_default();
            let path = cwd.join(&path_str);
            debug!("_ui_load_layout: loading {:?}", path);

            match std::fs::read_to_string(&path) {
                Ok(contents) => write_string_to_caller(&mut caller, &contents),
                Err(e) => {
                    error!("_ui_load_layout: Failed to read '{:?}': {}", path, e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        }
    );

    // _ui_load_page - Load an HTML page template. Caller provides the full relative path
    // (e.g. "app/ui/pages/index.html"). Path resolution is the caller's responsibility.
    register_bridge_fn!(
        linker,
        "_ui_load_page",
        |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
            let path_str = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    error!("_ui_load_page: Failed to read path");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            let cwd = std::env::current_dir().unwrap_or_default();
            let path = cwd.join(&path_str);
            debug!("_ui_load_page: loading {:?}", path);

            match std::fs::read_to_string(&path) {
                Ok(contents) => write_string_to_caller(&mut caller, &contents),
                Err(e) => {
                    error!("_ui_load_page: Failed to read '{:?}': {}", path, e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        }
    );

    // _ui_render_page - Render an HTML template with {{ key }} substitution.
    // Caller provides the full relative path (e.g. "app/ui/pages/index.html").
    // data is a JSON string; missing keys produce empty string.
    register_bridge_fn!(
        linker,
        "_ui_render_page",
        |mut caller: Caller<'_, WasmState>,
         name_ptr: i32,
         name_len: i32,
         data_ptr: i32,
         data_len: i32|
         -> i32 {
            let path_str = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    error!("_ui_render_page: Failed to read path");
                    return write_string_to_caller(&mut caller, "");
                }
            };

            let data_str = read_raw_string(&mut caller, data_ptr, data_len).unwrap_or_default();

            let cwd = std::env::current_dir().unwrap_or_default();
            let path = cwd.join(&path_str);
            debug!("_ui_render_page: rendering {:?}", path);

            let template = match std::fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(e) => {
                    error!("_ui_render_page: Failed to read '{:?}': {}", path, e);
                    return write_string_to_caller(&mut caller, "");
                }
            };

            let data: serde_json::Value = if data_str.is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&data_str).unwrap_or_else(|e| {
                    error!("_ui_render_page: Failed to parse JSON data for '{}': {}", path_str, e);
                    serde_json::Value::Object(serde_json::Map::new())
                })
            };

            // Extract <page layout="..."> wrapper if present
            let (page_content, layout_name) = extract_page_layout(&template);

            // Substitute template variables ({key} format)
            let substituted = substitute_template(&page_content, &data);

            // Evaluate server-side directives (cl-if, cl-show, cl-iterate)
            let with_directives = process_directives(&substituted, &data);

            // Wrap in layout if specified
            let rendered = match layout_name {
                Some(ref name) => apply_layout(&with_directives, name, &cwd),
                None => with_directives,
            };

            caller.data_mut().add_header(
                "Content-Type".to_string(),
                "text/html; charset=utf-8".to_string(),
            );
            write_string_to_caller(&mut caller, &rendered)
        }
    );

    // _ui_inject_head_css - Accumulate CSS for injection into the response <head>
    register_bridge_fn!(
        linker,
        "_ui_inject_head_css",
        |mut caller: Caller<'_, WasmState>, css_ptr: i32, css_len: i32| -> i32 {
            let css = match read_raw_string(&mut caller, css_ptr, css_len) {
                Some(s) => s,
                None => {
                    error!("_ui_inject_head_css: Failed to read CSS string");
                    return 0;
                }
            };

            caller.data_mut().pending_head_css.push(css);
            1
        }
    );

    // _ui_inject_head_link - Inject <link rel="stylesheet" href="..."> into response <head>
    // Deduplicated: the same href injected multiple times produces a single <link> tag.
    register_bridge_fn!(
        linker,
        "_ui_inject_head_link",
        |mut caller: Caller<'_, WasmState>, href_ptr: i32, href_len: i32| -> i32 {
            let href = match read_raw_string(&mut caller, href_ptr, href_len) {
                Some(s) => s,
                None => {
                    error!("_ui_inject_head_link: Failed to read href string");
                    return 0;
                }
            };

            let links = &mut caller.data_mut().pending_head_links;
            if !links.contains(&href) {
                links.push(href);
            }
            1
        }
    );

    Ok(())
}

/// Replace `{key}` tokens in `template` with values from `data`.
/// Missing keys are left as-is. Only top-level JSON object fields are supported.
fn substitute_template(template: &str, data: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = data.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}

/// Extract `<page layout="name">...</page>` wrapper.
/// Returns (inner_content, Some(layout_name)) or (full_html, None).
fn extract_page_layout(html: &str) -> (String, Option<String>) {
    const PAGE_TAG: &str = "<page";
    let Some(tag_start) = html.find(PAGE_TAG) else {
        return (html.to_string(), None);
    };

    let after_tag = &html[tag_start + PAGE_TAG.len()..];

    let layout = after_tag.find(" layout=\"").map(|attr_pos| {
        let val_start = attr_pos + " layout=\"".len();
        let val_end = after_tag[val_start..].find('"').unwrap_or(0);
        after_tag[val_start..val_start + val_end].to_string()
    });

    let open_end = html[tag_start..]
        .find('>')
        .map(|p| tag_start + p + 1)
        .unwrap_or(tag_start);

    let close_tag = "</page>";
    let inner_end = html[open_end..].find(close_tag).map(|p| open_end + p).unwrap_or(html.len());
    let inner = html[open_end..inner_end].trim().to_string();

    (inner, layout)
}

/// Load a layout file and inject `content` into its `<slot>`.
fn apply_layout(content: &str, layout_name: &str, cwd: &std::path::Path) -> String {
    let candidates = [
        cwd.join(format!("app/ui/layouts/{}.html", layout_name)),
        cwd.join(format!("ui/layouts/{}.html", layout_name)),
        cwd.join(format!("layouts/{}.html", layout_name)),
        cwd.join(format!("{}.html", layout_name)),
    ];

    for path in &candidates {
        if let Ok(layout_html) = std::fs::read_to_string(path) {
            return layout_html
                .replace("<slot />", content)
                .replace("<slot/>", content)
                .replace("<slot></slot>", content);
        }
    }

    debug!("apply_layout: layout '{}' not found, returning content as-is", layout_name);
    content.to_string()
}

/// Evaluate server-side directives: cl-iterate, cl-if/cl-else, cl-show.
fn process_directives(html: &str, data: &serde_json::Value) -> String {
    let mut result = process_iterate_directive(html, data);
    result = process_if_directive(&result, data);
    result = process_show_directive(&result, data);
    result
}

/// Look up a dot-separated path in a JSON value.
fn get_nested_value<'a>(data: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = data;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Evaluate a simple boolean condition against data (key path lookup).
fn evaluate_condition(condition: &str, data: &serde_json::Value) -> bool {
    match get_nested_value(data, condition.trim()) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Null) | None => false,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
        Some(serde_json::Value::Object(obj)) => !obj.is_empty(),
    }
}

/// Find the opening tag start (`<`) that contains position `attr_pos` inside `html`.
fn find_tag_start(html: &str, attr_pos: usize) -> Option<usize> {
    html[..attr_pos].rfind('<')
}

/// Extract the tag name from an opening tag starting at `tag_start` in `html`.
fn extract_tag_name(html: &str, tag_start: usize) -> String {
    let after = &html[tag_start + 1..];
    let end = after
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(after.len());
    after[..end].to_string()
}

/// Find the end position (exclusive) of the element that begins with an opening tag at `tag_start`.
/// Handles nesting by counting open/close pairs for `tag_name`.
fn find_element_end(html: &str, tag_start: usize, tag_name: &str) -> Option<usize> {
    let open_pattern = format!("<{}", tag_name);
    let close_pattern = format!("</{}>", tag_name);

    // Skip past the opening tag itself
    let open_tag_end = html[tag_start..].find('>')? + tag_start + 1;
    let mut depth = 1usize;
    let mut pos = open_tag_end;

    while pos < html.len() {
        let rest = &html[pos..];
        let next_open = rest.find(&open_pattern);
        let next_close = rest.find(&close_pattern);

        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                pos += o + open_pattern.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                if depth == 0 {
                    return Some(pos + c + close_pattern.len());
                }
                pos += c + close_pattern.len();
            }
            (Some(o), None) => {
                depth += 1;
                pos += o + open_pattern.len();
            }
            (None, None) => break,
        }
    }
    None
}

/// Process `cl-iterate="item in array_path"` directives.
fn process_iterate_directive(html: &str, data: &serde_json::Value) -> String {
    const MARKER: &str = " cl-iterate=\"";
    let mut result = html.to_string();

    while let Some(attr_pos) = result.find(MARKER) {

        let tag_start = match find_tag_start(&result, attr_pos) {
            Some(p) => p,
            None => break,
        };
        let tag_name = extract_tag_name(&result, tag_start);

        // Extract attribute value
        let val_start = attr_pos + MARKER.len();
        let val_end = match result[val_start..].find('"') {
            Some(p) => val_start + p,
            None => break,
        };
        let attr_value = result[val_start..val_end].to_string();

        // Parse "item in array_path"
        let parts: Vec<&str> = attr_value.splitn(3, ' ').collect();
        if parts.len() != 3 || parts[1] != "in" {
            break;
        }
        let item_var = parts[0];
        let array_path = parts[2];

        // Find element end
        let element_end = match find_element_end(&result, tag_start, &tag_name) {
            Some(e) => e,
            None => break,
        };

        // Extract inner HTML (between opening tag close and closing tag)
        let open_tag_end = match result[tag_start..].find('>') {
            Some(p) => tag_start + p + 1,
            None => break,
        };
        let close_tag_len = tag_name.len() + 3; // </name>
        let inner = result[open_tag_end..element_end - close_tag_len].to_string();

        // Get array from data
        let items: Vec<serde_json::Value> = match get_nested_value(data, array_path) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => vec![],
        };

        // Expand
        let mut expanded = String::new();
        for item in &items {
            let mut item_html = inner.clone();
            if let Some(obj) = item.as_object() {
                for (field, value) in obj {
                    let placeholder = format!("{{{}.{}}}", item_var, field);
                    let replacement = match value {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    item_html = item_html.replace(&placeholder, &replacement);
                }
            }
            // Scalar items: {item_var}
            let placeholder = format!("{{{}}}", item_var);
            let scalar = match item {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            item_html = item_html.replace(&placeholder, &scalar);
            expanded.push_str(&item_html);
        }

        result = format!("{}{}{}", &result[..tag_start], expanded, &result[element_end..]);
    }

    result
}

/// Process `cl-if="condition"` / `cl-else` directives.
fn process_if_directive(html: &str, data: &serde_json::Value) -> String {
    const MARKER: &str = " cl-if=\"";
    let mut result = html.to_string();

    while let Some(attr_pos) = result.find(MARKER) {

        let tag_start = match find_tag_start(&result, attr_pos) {
            Some(p) => p,
            None => break,
        };
        let tag_name = extract_tag_name(&result, tag_start);

        let val_start = attr_pos + MARKER.len();
        let val_end = match result[val_start..].find('"') {
            Some(p) => val_start + p,
            None => break,
        };
        let condition = result[val_start..val_end].to_string();
        let is_truthy = evaluate_condition(&condition, data);

        let element_end = match find_element_end(&result, tag_start, &tag_name) {
            Some(e) => e,
            None => break,
        };
        let open_tag_end = match result[tag_start..].find('>') {
            Some(p) => tag_start + p + 1,
            None => break,
        };
        let close_tag_len = tag_name.len() + 3;
        let inner = result[open_tag_end..element_end - close_tag_len].to_string();

        // Check for a cl-else element immediately following (ignoring whitespace)
        let rest_trimmed_offset = element_end
            + result[element_end..].len()
            - result[element_end..].trim_start().len();
        let rest = &result[rest_trimmed_offset..];
        let has_else = rest.starts_with('<') && {
            let tag_end = rest.find('>').unwrap_or(0);
            rest[..tag_end].contains(" cl-else")
        };

        let (keep, total_end) = if has_else {
            // Find the else element
            let else_tag_start = rest_trimmed_offset;
            let else_tag_name = extract_tag_name(&result, else_tag_start);
            let else_element_end = find_element_end(&result, else_tag_start, &else_tag_name)
                .unwrap_or(else_tag_start);
            let else_open_tag_end = result[else_tag_start..].find('>').map(|p| else_tag_start + p + 1).unwrap_or(else_tag_start);
            let else_close_tag_len = else_tag_name.len() + 3;
            let else_inner = result[else_open_tag_end..else_element_end - else_close_tag_len].to_string();

            if is_truthy {
                (inner, else_element_end)
            } else {
                (else_inner, else_element_end)
            }
        } else if is_truthy {
            (inner, element_end)
        } else {
            (String::new(), element_end)
        };

        result = format!("{}{}{}", &result[..tag_start], keep, &result[total_end..]);
    }

    result
}

/// Process `cl-show="condition"` directives (adds `display:none` when falsy).
fn process_show_directive(html: &str, data: &serde_json::Value) -> String {
    const MARKER: &str = " cl-show=\"";
    let mut result = html.to_string();

    while let Some(attr_pos) = result.find(MARKER) {

        let val_start = attr_pos + MARKER.len();
        let val_end = match result[val_start..].find('"') {
            Some(p) => val_start + p,
            None => break,
        };
        let condition = result[val_start..val_end].to_string();
        let is_truthy = evaluate_condition(&condition, data);

        // Remove the cl-show="..." attribute regardless
        let attr_full = format!(" cl-show=\"{}\"", condition);
        if is_truthy {
            result = result.replacen(&attr_full, "", 1);
        } else {
            // Find the tag start and add display:none style
            let tag_start = match find_tag_start(&result, attr_pos) {
                Some(p) => p,
                None => break,
            };
            let tag_end = match result[tag_start..].find('>') {
                Some(p) => tag_start + p,
                None => break,
            };
            let opening_tag = result[tag_start..tag_end + 1].to_string();
            let new_tag = if opening_tag.contains("style=\"") {
                opening_tag
                    .replacen(&attr_full, "", 1)
                    .replacen("style=\"", "style=\"display:none;", 1)
            } else {
                opening_tag
                    .replacen(&attr_full, "", 1)
                    .replacen('>', " style=\"display:none;\">", 1)
            };
            result = format!("{}{}{}", &result[..tag_start], new_tag, &result[tag_end + 1..]);
        }
    }

    result
}

/// Register async bridge functions (_async_fire, _async_await, _server_sleep)
fn register_async_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _async_fire — fire-and-forget: lower `background expr`
    // Signature: (fn_name_ptr: i32, fn_name_len: i32, args_ptr: i32, args_len: i32) -> void
    register_bridge_fn!(
        linker,
        "_async_fire",
        |mut caller: Caller<'_, WasmState>,
         fn_name_ptr: i32,
         fn_name_len: i32,
         _args_ptr: i32,
         _args_len: i32| {
            let fn_name = read_raw_string(&mut caller, fn_name_ptr, fn_name_len)
                .unwrap_or_else(|| "<unknown>".to_string());
            debug!("_async_fire: scheduling background call to '{}'", fn_name);
        }
    );

    // _async_await — blocking async: lower `later x = expr`
    // Signature: (fn_name_ptr: i32, fn_name_len: i32, args_ptr: i32, args_len: i32) -> i32 (ptr)
    register_bridge_fn!(
        linker,
        "_async_await",
        |mut caller: Caller<'_, WasmState>,
         fn_name_ptr: i32,
         fn_name_len: i32,
         _args_ptr: i32,
         _args_len: i32|
         -> i32 {
            let fn_name = read_raw_string(&mut caller, fn_name_ptr, fn_name_len)
                .unwrap_or_else(|| "<unknown>".to_string());
            debug!("_async_await: blocking call to '{}'", fn_name);
            // Write an empty string result into WASM memory and return its pointer
            write_string_to_caller(&mut caller, "")
        }
    );

    // _server_sleep — suspend for N milliseconds
    // Signature: (ms: i64) -> void
    register_bridge_fn!(
        linker,
        "_server_sleep",
        |_caller: Caller<'_, WasmState>, ms: i64| {
            let duration = std::time::Duration::from_millis(ms.max(0) as u64);
            debug!("_server_sleep: sleeping for {}ms", ms);
            std::thread::sleep(duration);
        }
    );

    Ok(())
}

/// Register MCP bridge functions (_mcp_stdio_read, _mcp_stdio_write, _mcp_http_serve,
/// _mcp_http_accept, _mcp_sse_send, _mcp_log)
fn register_mcp_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _mcp_stdio_read — blocks reading one newline-terminated JSON-RPC message from stdin
    register_bridge_fn!(
        linker,
        "_mcp_stdio_read",
        |mut caller: Caller<'_, WasmState>| -> i32 {
            use std::io::BufRead;
            let mut line = String::new();
            match std::io::stdin().lock().read_line(&mut line) {
                Ok(0) => write_string_to_caller(&mut caller, ""),
                Ok(_) => {
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    debug!("_mcp_stdio_read: {} bytes", line.len());
                    write_string_to_caller(&mut caller, &line)
                }
                Err(e) => {
                    error!("_mcp_stdio_read: {}", e);
                    write_string_to_caller(&mut caller, "")
                }
            }
        }
    );

    // _mcp_stdio_write — write to stdout (stdio mode) or pending HTTP response (HTTP mode)
    register_bridge_fn!(
        linker,
        "_mcp_stdio_write",
        |mut caller: Caller<'_, WasmState>, msg_ptr: i32, msg_len: i32| -> i32 {
            let msg = match read_raw_string(&mut caller, msg_ptr, msg_len) {
                Some(s) => s,
                None => {
                    error!("_mcp_stdio_write: failed to read message");
                    return 0;
                }
            };
            let mcp = caller.data().mcp.clone();
            let transport = mcp.transport.lock().expect("mcp transport lock").clone();
            match transport {
                McpTransport::Stdio => {
                    use std::io::Write;
                    let mut stdout = std::io::stdout().lock();
                    if stdout
                        .write_all(format!("{}\n", msg).as_bytes())
                        .is_ok()
                    {
                        let _ = stdout.flush();
                        debug!("_mcp_stdio_write: stdio {} bytes", msg.len());
                        1
                    } else {
                        0
                    }
                }
                McpTransport::Http => {
                    let tx = mcp
                        .current_http_response
                        .lock()
                        .expect("mcp response lock")
                        .take();
                    match tx {
                        Some(sender) => {
                            debug!("_mcp_stdio_write: http {} bytes", msg.len());
                            if sender.send(msg).is_ok() { 1 } else { 0 }
                        }
                        None => {
                            error!("_mcp_stdio_write: no pending HTTP response context");
                            0
                        }
                    }
                }
            }
        }
    );

    // _mcp_http_serve — start background MCP HTTP+SSE server
    register_bridge_fn!(
        linker,
        "_mcp_http_serve",
        |mut caller: Caller<'_, WasmState>, port: i32, host_ptr: i32, host_len: i32| -> i32 {
            let host = read_raw_string(&mut caller, host_ptr, host_len)
                .unwrap_or_else(|| "0.0.0.0".to_string());
            let mcp = caller.data().mcp.clone();
            *mcp.transport.lock().expect("mcp transport lock") = McpTransport::Http;
            let addr = format!("{}:{}", host, port);
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(run_mcp_http_server(addr.clone(), mcp));
                    info!("_mcp_http_serve: MCP HTTP server starting on {}", addr);
                    1
                }
                Err(e) => {
                    error!("_mcp_http_serve: no tokio runtime: {}", e);
                    0
                }
            }
        }
    );

    // _mcp_http_accept — block until next POST /mcp request, return body
    register_bridge_fn!(
        linker,
        "_mcp_http_accept",
        |mut caller: Caller<'_, WasmState>| -> i32 {
            let mcp = caller.data().mcp.clone();
            let request = {
                let (ref queue_mutex, ref condvar) = mcp.request_queue;
                let mut q = queue_mutex.lock().expect("mcp queue lock");
                loop {
                    if let Some(req) = q.pop_front() {
                        break req;
                    }
                    q = condvar.wait(q).expect("mcp condvar wait");
                }
            };
            *mcp.current_http_response.lock().expect("mcp response lock") =
                Some(request.response_tx);
            debug!("_mcp_http_accept: received request {} bytes", request.body.len());
            write_string_to_caller(&mut caller, &request.body)
        }
    );

    // _mcp_sse_send — send raw SSE-formatted event to a specific connected client
    register_bridge_fn!(
        linker,
        "_mcp_sse_send",
        |mut caller: Caller<'_, WasmState>,
         client_id_ptr: i32,
         client_id_len: i32,
         event_ptr: i32,
         event_len: i32|
         -> i32 {
            let client_id = match read_raw_string(&mut caller, client_id_ptr, client_id_len) {
                Some(s) => s,
                None => return 0,
            };
            let event = match read_raw_string(&mut caller, event_ptr, event_len) {
                Some(s) => s,
                None => return 0,
            };
            let clients = caller.data().mcp.sse_clients.lock().expect("sse clients lock");
            match clients.get(&client_id) {
                Some(tx) => {
                    if tx.send(event).is_ok() {
                        debug!("_mcp_sse_send: sent to client {}", client_id);
                        1
                    } else {
                        debug!("_mcp_sse_send: client {} disconnected", client_id);
                        0
                    }
                }
                None => {
                    debug!("_mcp_sse_send: unknown client {}", client_id);
                    0
                }
            }
        }
    );

    // _mcp_log — write structured log to stderr (never stdout, which would corrupt stdio transport)
    register_bridge_fn!(
        linker,
        "_mcp_log",
        |mut caller: Caller<'_, WasmState>,
         level_ptr: i32,
         level_len: i32,
         msg_ptr: i32,
         msg_len: i32|
         -> i32 {
            let level = read_raw_string(&mut caller, level_ptr, level_len)
                .unwrap_or_else(|| "info".to_string());
            let msg = read_raw_string(&mut caller, msg_ptr, msg_len).unwrap_or_default();
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "[frame.mcp] {}: {}", level, msg);
            1
        }
    );

    Ok(())
}

// ── MCP HTTP server helpers ──────────────────────────────────────────────────

/// Axum router state for the MCP HTTP server
type McpAxumState = std::sync::Arc<McpBridgeState>;

/// Run an MCP HTTP+SSE server on the given address.
///
/// Called by `_mcp_http_serve` on the current tokio runtime.
async fn run_mcp_http_server(addr: String, mcp: McpAxumState) {
    use axum::{Router, routing::{get, post}};
    use tower_http::cors::{Any, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers(Any);

    let app = Router::new()
        .route("/mcp", post(handle_mcp_post))
        .route("/sse", get(handle_mcp_sse))
        .layer(cors)
        .with_state(mcp);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("MCP HTTP server: failed to bind {}: {}", addr, e);
            return;
        }
    };

    tracing::info!("MCP HTTP server listening on {}", addr);
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("MCP HTTP server error: {}", e);
    }
}

/// Handle POST /mcp — queue the request body for the WASM module, await the response.
async fn handle_mcp_post(
    axum::extract::State(mcp): axum::extract::State<McpAxumState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let body_str = String::from_utf8_lossy(&body).into_owned();
    let session_id = uuid::Uuid::new_v4().to_string();

    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1);
    {
        let (ref queue_mutex, ref condvar) = mcp.request_queue;
        let mut queue = queue_mutex.lock().expect("mcp queue lock");
        queue.push_back(McpPendingRequest {
            body: body_str,
            response_tx: tx,
        });
        condvar.notify_one();
    }

    let response_body = tokio::task::spawn_blocking(move || rx.recv().unwrap_or_default())
        .await
        .unwrap_or_default();

    axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .header("Mcp-Session-Id", session_id)
        .body(axum::body::Body::from(response_body))
        .expect("valid MCP POST response")
}

/// Handle GET /sse — register a new SSE client and stream events to it.
async fn handle_mcp_sse(
    axum::extract::State(mcp): axum::extract::State<McpAxumState>,
) -> axum::response::Response {
    let client_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    mcp.sse_clients
        .lock()
        .expect("sse clients lock")
        .insert(client_id.clone(), tx);

    tracing::info!("MCP SSE client connected: {}", client_id);

    let stream = McpSseStream { rx };

    axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Access-Control-Allow-Origin", "*")
        .header("X-Accel-Buffering", "no")
        .header("Mcp-Session-Id", client_id)
        .body(axum::body::Body::from_stream(stream))
        .expect("valid SSE response")
}

/// Stream adapter that converts a tokio unbounded channel receiver into an
/// `http_body` `Stream` usable by `axum::body::Body::from_stream`.
struct McpSseStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl futures::Stream for McpSseStream {
    type Item = Result<bytes::Bytes, std::convert::Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(s)) => {
                std::task::Poll::Ready(Some(Ok(bytes::Bytes::from(s))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Register test bridge functions (_test_http_request, _test_response_status, _test_response_body).
///
/// These support `test: endpoint:` blocks in Clean Language. `_test_http_request` dispatches
/// an in-process HTTP request by calling the registered route handler WASM export directly,
/// capturing the response into a handle map. The companion functions read back status/body.
fn register_test_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _test_http_request - Dispatch an in-process request and return a response handle
    // Signature: (method_ptr, method_len, path_ptr, path_len, body_ptr, body_len,
    //             hkey_ptr, hkey_len, hval_ptr, hval_len) -> i32
    // Returns: handle >= 0 on success, -1 if route not found or dispatch fails
    register_bridge_fn!(
        linker,
        "_test_http_request",
        |mut caller: Caller<'_, WasmState>,
         method_ptr: i32,
         method_len: i32,
         path_ptr: i32,
         path_len: i32,
         body_ptr: i32,
         body_len: i32,
         hkey_ptr: i32,
         hkey_len: i32,
         hval_ptr: i32,
         hval_len: i32|
         -> i32 {
            let method = read_raw_string(&mut caller, method_ptr, method_len)
                .unwrap_or_else(|| "GET".to_string());
            let full_path = read_raw_string(&mut caller, path_ptr, path_len)
                .unwrap_or_else(|| "/".to_string());
            let body = read_raw_string(&mut caller, body_ptr, body_len).unwrap_or_default();
            let hkey = read_raw_string(&mut caller, hkey_ptr, hkey_len).unwrap_or_default();
            let hval = read_raw_string(&mut caller, hval_ptr, hval_len).unwrap_or_default();

            // Strip query string from path for route lookup
            let (clean_path, query_str) = match full_path.find('?') {
                Some(qi) => (full_path[..qi].to_string(), full_path[qi + 1..].to_string()),
                None => (full_path.clone(), String::new()),
            };

            let http_method = match HttpMethod::parse(&method) {
                Ok(m) => m,
                Err(_) => {
                    error!("_test_http_request: invalid method '{}'", method);
                    return -1;
                }
            };

            // Look up route and extract path params
            let (handler_name, path_params) = {
                let state = caller.data();
                match state.router.find(http_method, &clean_path) {
                    Some((handler, params)) => (handler.handler_name.clone(), params),
                    None => {
                        debug!("_test_http_request: no route for {} {}", method, clean_path);
                        return -1;
                    }
                }
            };

            // Get the handler export before mutably borrowing caller
            let handler_func = caller.get_export(&handler_name).and_then(|e| e.into_func());
            let handler_func = match handler_func {
                Some(f) => f,
                None => {
                    error!(
                        "_test_http_request: handler export '{}' not found",
                        handler_name
                    );
                    return -1;
                }
            };

            // Parse query string into a map
            let query: std::collections::HashMap<String, String> = if query_str.is_empty() {
                std::collections::HashMap::new()
            } else {
                use url::form_urlencoded;
                form_urlencoded::parse(query_str.as_bytes())
                    .into_owned()
                    .collect()
            };

            // Build request headers
            let mut headers = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
            ];
            if !hkey.is_empty() {
                headers.push((hkey, hval));
            }

            // Set test request context and clear pending response state
            {
                let state = caller.data_mut();
                state.request_context = Some(crate::wasm::RequestContext {
                    method: method.clone(),
                    path: clean_path.clone(),
                    headers,
                    body,
                    params: path_params,
                    query,
                });
                state.pending_status = None;
                state.pending_body = None;
                state.pending_headers.clear();
                state.pending_redirect = None;
                state.auth_context = None;
            }

            // Determine result buffer size and call the handler
            let result_count = handler_func.ty(&caller).results().len();
            let mut results = vec![wasmtime::Val::I32(0); result_count];
            let call_ok = handler_func.call(&mut caller, &[], &mut results).is_ok();

            // Capture and store the response
            let handle = {
                let state = caller.data_mut();
                let status = state.pending_status.unwrap_or(200) as i32;
                let body = state.pending_body.clone().unwrap_or_default();
                // Clear request state
                state.request_context = None;
                state.pending_status = None;
                state.pending_body = None;
                state.pending_headers.clear();
                state.auth_context = None;

                if !call_ok {
                    debug!("_test_http_request: handler '{}' returned an error", handler_name);
                    return -1;
                }

                let h = state.next_test_handle;
                state.next_test_handle += 1;
                state.test_responses.insert(h, TestResponse { status, body });
                h
            };

            debug!(
                "_test_http_request: {} {} -> handle {}",
                method, clean_path, handle
            );
            handle
        }
    );

    // _test_response_status - Get HTTP status from a test response handle
    // Signature: (handle: i32) -> i32
    // Returns: status code (e.g. 200), or -1 if handle unknown
    register_bridge_fn!(
        linker,
        "_test_response_status",
        |caller: Caller<'_, WasmState>, handle: i32| -> i32 {
            let state = caller.data();
            match state.test_responses.get(&handle) {
                Some(r) => r.status,
                None => {
                    debug!("_test_response_status: unknown handle {}", handle);
                    -1
                }
            }
        }
    );

    // _test_response_body - Get response body JSON from a test response handle
    // Signature: (handle: i32) -> i32 (length-prefixed pointer)
    // Returns: empty string if handle unknown
    register_bridge_fn!(
        linker,
        "_test_response_body",
        |mut caller: Caller<'_, WasmState>, handle: i32| -> i32 {
            let body = {
                let state = caller.data();
                match state.test_responses.get(&handle) {
                    Some(r) => r.body.clone(),
                    None => {
                        debug!("_test_response_body: unknown handle {}", handle);
                        String::new()
                    }
                }
            };
            write_string_to_caller(&mut caller, &body)
        }
    );

    Ok(())
}

/// Register dot-notation aliases for all Layer 3 `_namespace_fn` bridge functions.
///
/// The Clean Language compiler (0.30.120+) generates WASM imports in both
/// `_namespace_fn` and `namespace.fn` styles. This registers the dot form as
/// an alias of the already-registered underscore form so both resolve at link time.
///
/// Derived from function-registry.toml `aliases` field (Layer 3 entries).
fn register_dot_aliases(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    const ALIASES: &[(&str, &str)] = &[
        // HTTP server (register_http_server_functions)
        ("_http_listen",         "http.listen"),
        ("_http_route",          "http.route"),
        ("_http_route_protected","http.route_protected"),
        ("_http_serve_static",   "http.serve_static"),
        // Islands (register_islands_functions)
        ("_island_register",     "island.register"),
        // Request context (register_request_context_functions)
        ("_req_param",           "req.param"),
        ("_req_param_int",       "req.param_int"),
        ("_req_query",           "req.query"),
        ("_req_body",            "req.body"),
        ("_req_body_field",      "req.body_field"),
        ("_req_header",          "req.header"),
        ("_req_headers",         "req.headers"),
        ("_req_method",          "req.method"),
        ("_req_path",            "req.path"),
        ("_req_cookie",          "req.cookie"),
        ("_req_form",            "req.form"),
        ("_req_ip",              "req.ip"),
        // Session management (register_session_management_functions)
        ("_session_store",       "session.store"),
        ("_session_get",         "session.get"),
        ("_session_delete",      "session.delete"),
        ("_session_exists",      "session.exists"),
        ("_session_set_csrf",    "session.set_csrf"),
        ("_session_get_csrf",    "session.get_csrf"),
        ("_http_set_cookie",     "http.set_cookie"),
        // Auth (register_session_auth_functions)
        ("_auth_get_session",    "auth.get_session"),
        ("_auth_require_auth",   "auth.require_auth"),
        ("_auth_require_role",   "auth.require_role"),
        ("_auth_can",            "auth.can"),
        ("_auth_has_any_role",   "auth.has_any_role"),
        ("_auth_set_session",    "auth.set_session"),
        ("_auth_clear_session",  "auth.clear_session"),
        ("_auth_user_id",        "auth.user_id"),
        ("_auth_user_role",      "auth.user_role"),
        // Roles (register_roles_functions)
        ("_roles_register",      "roles.register"),
        ("_role_has_permission", "role.has_permission"),
        ("_role_get_permissions","role.get_permissions"),
        // Response (register_response_functions)
        ("_http_respond",        "http.respond"),
        ("_http_redirect",       "http.redirect"),
        ("_http_set_header",     "http.set_header"),
        ("_res_set_header",      "res.set_header"),
        ("_res_redirect",        "res.redirect"),
        ("_res_status",          "res.status"),
        ("_res_body",            "res.body"),
        ("_res_json",            "res.json"),
        ("_http_set_cache",      "http.set_cache"),
        ("_http_no_cache",       "http.no_cache"),
        ("_json_encode",         "json.encode"),
        ("_json_decode",         "json.decode"),
        ("_json_get",            "json.get"),
        // Async aliases are registered by register_bridge_fn! macro in register_async_functions
        // Test bridge aliases are registered by register_bridge_fn! macro in register_test_functions
    ];

    for (canonical, dot_alias) in ALIASES {
        linker
            .alias("env", canonical, "env", dot_alias)
            .map_err(|e| RuntimeError::wasm(format!("Failed to alias {} -> {}: {}", canonical, dot_alias, e)))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_linker() {
        let engine = Engine::default();
        // This will fail because WasmState requires a router, but the linker creation should work
        let result = create_linker(&engine);
        assert!(result.is_ok());
    }

    #[test]
    fn test_json_get_path_logic() {
        // Tests the path traversal logic used by _json_get, including numeric array indices
        fn json_get_by_path<'a>(
            parsed: &'a serde_json::Value,
            path: &str,
        ) -> Option<&'a serde_json::Value> {
            let mut current = parsed;
            for part in path.split('.') {
                let next = if let Ok(idx) = part.parse::<usize>() {
                    current.get(idx)
                } else {
                    current.get(part)
                };
                current = next?;
            }
            Some(current)
        }

        let parsed = serde_json::json!({
            "ok": true,
            "data": {
                "count": 4,
                "rows": [
                    {"slug": "post-1", "read_time": 5},
                    {"slug": "post-2", "read_time": 8}
                ]
            }
        });

        // Object access
        assert_eq!(json_get_by_path(&parsed, "ok"), Some(&serde_json::json!(true)));
        assert_eq!(json_get_by_path(&parsed, "data.count"), Some(&serde_json::json!(4)));

        // Array indexing via numeric path segments
        let slug0 = json_get_by_path(&parsed, "data.rows.0.slug").unwrap();
        assert_eq!(slug0, &serde_json::json!("post-1"));

        let slug1 = json_get_by_path(&parsed, "data.rows.1.slug").unwrap();
        assert_eq!(slug1, &serde_json::json!("post-2"));

        let rt0 = json_get_by_path(&parsed, "data.rows.0.read_time").unwrap();
        assert_eq!(rt0, &serde_json::json!(5));

        // Out-of-bounds returns None
        assert!(json_get_by_path(&parsed, "data.rows.5.slug").is_none());

        // Missing key returns None
        assert!(json_get_by_path(&parsed, "data.missing").is_none());
    }

    // --- Registry TOML types ---

    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    struct Registry {
        meta: RegistryMeta,
        functions: Vec<FunctionEntry>,
    }

    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    struct RegistryMeta {
        version: String,
        generated_from: Vec<String>,
    }

    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    struct FunctionEntry {
        name: String,
        layer: u32,
        category: String,
        module: String,
        params: Vec<String>,
        returns: String,
        #[serde(default)]
        aliases: Vec<String>,
        description: String,
    }

    fn expand_param_type(t: &str) -> Vec<&str> {
        match t {
            "string" => vec!["i32", "i32"],
            "integer" => vec!["i64"],
            "number" => vec!["f64"],
            "boolean" => vec!["i32"],
            "i32" => vec!["i32"],
            "i64" => vec!["i64"],
            other => panic!("Unknown param type in registry: '{}'", other),
        }
    }

    fn expand_return_type(t: &str) -> Option<&str> {
        match t {
            "void" => None,
            "ptr" => Some("i32"),
            "i32" => Some("i32"),
            "i64" => Some("i64"),
            "boolean" => Some("i32"),
            "integer" => Some("i64"),
            "number" => Some("f64"),
            other => panic!("Unknown return type in registry: '{}'", other),
        }
    }

    fn generate_wat_import(module: &str, name: &str, params: &[String], returns: &str) -> String {
        let mut import = format!("  (import \"{}\" \"{}\" (func", module, name);

        let wasm_params: Vec<&str> = params.iter()
            .flat_map(|t| expand_param_type(t))
            .collect();

        if !wasm_params.is_empty() {
            import.push_str(" (param");
            for p in &wasm_params {
                import.push_str(&format!(" {}", p));
            }
            import.push(')');
        }

        if let Some(ret) = expand_return_type(returns) {
            import.push_str(&format!(" (result {})", ret));
        }

        import.push_str("))\n");
        import
    }

    /// Layer 3 spec compliance test: validates that ALL server-specific host
    /// function signatures match the shared function registry.
    ///
    /// This is the Layer 3 counterpart to host-bridge's Layer 2 test.
    /// It generates WAT imports for Layer 3 functions and instantiates
    /// against the full server linker (which includes both L2 and L3 functions).
    #[test]
    fn test_layer3_spec_compliance() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let registry_path = std::path::Path::new(manifest_dir)
            .join("../foundation/platform-architecture/function-registry.toml");
        let toml_str = std::fs::read_to_string(&registry_path)
            .unwrap_or_else(|e| panic!(
                "Failed to read function-registry.toml at {:?}: {}",
                registry_path, e
            ));

        let registry: Registry = toml::from_str(&toml_str)
            .expect("Failed to parse function-registry.toml");

        // Filter for Layer 3 functions only (server-specific scope)
        let layer3_funcs: Vec<&FunctionEntry> = registry.functions.iter()
            .filter(|f| f.layer == 3)
            .collect();

        assert!(
            layer3_funcs.len() >= 30,
            "Expected at least 30 Layer 3 canonical functions in registry, found {}",
            layer3_funcs.len()
        );

        // Generate WAT module with all Layer 3 imports
        let mut wat = String::from("(module\n");
        let mut import_count = 0;

        for func in &layer3_funcs {
            wat.push_str(&generate_wat_import(&func.module, &func.name, &func.params, &func.returns));
            import_count += 1;

            for alias in &func.aliases {
                wat.push_str(&generate_wat_import(&func.module, alias, &func.params, &func.returns));
                import_count += 1;
            }
        }

        wat.push_str(")\n");

        // Create full server linker (includes L2 + L3) and validate
        let engine = Engine::default();
        let linker = create_linker(&engine).expect("Failed to create linker");
        let module = wasmtime::Module::new(&engine, &wat)
            .unwrap_or_else(|e| panic!(
                "Failed to parse generated WAT ({} imports): {}\n\nGenerated WAT:\n{}",
                import_count, e, wat
            ));

        let router = std::sync::Arc::new(crate::router::Router::new());
        let state = WasmState::new(router);
        let mut store = wasmtime::Store::new(&engine, state);

        linker.instantiate(&mut store, &module).unwrap_or_else(|e| panic!(
            "LAYER 3 SPEC COMPLIANCE FAILURE ({} imports):\n{}\n\n\
             Fix the implementation to match function-registry.toml, not the other way around.",
            import_count, e
        ));

        eprintln!(
            "Layer 3 spec compliance PASSED: {} canonical + aliases = {} total imports",
            layer3_funcs.len(), import_count
        );
    }
}
