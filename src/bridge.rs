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
//! - HTTP server (_http_listen, _http_route, _http_route_protected)
//! - Request context (_req_param, _req_query, _req_body, _req_header, _req_method, _req_path, _req_cookie)
//! - Response manipulation (_res_set_header, _res_redirect)
//! - Session management (_session_create, _session_get, _session_destroy, _session_set_cookie)
//! - Session auth (_auth_get_session, _auth_require_auth, _auth_require_role, _auth_can, _auth_has_any_role)

use crate::error::{RuntimeError, RuntimeResult};
use crate::router::HttpMethod;
use crate::session::parse_cookies;
use crate::wasm::WasmState;
use host_bridge::{read_raw_string, write_string_to_caller};
use tracing::{debug, error, info};
use wasmtime::{Caller, Engine, Linker};

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
    register_response_functions(&mut linker)?;

    Ok(linker)
}

/// Register HTTP server functions (_http_listen, _http_route, _http_route_protected)
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
    linker
        .func_wrap(
            "env",
            "_http_route",
            |mut caller: Caller<'_, WasmState>,
             method_ptr: i32,
             method_len: i32,
             path_ptr: i32,
             path_len: i32,
             handler_idx: i32|
             -> i32 {
                let method_str = read_raw_string(&mut caller, method_ptr, method_len)
                    .unwrap_or_else(|| "GET".to_string());
                let path = read_raw_string(&mut caller, path_ptr, path_len)
                    .unwrap_or_else(|| "/".to_string());

                debug!(
                    "_http_route: method={}, path={}, handler={}",
                    method_str, path, handler_idx
                );

                let method = match HttpMethod::from_str(&method_str) {
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
                    router.register(method, path.clone(), handler_idx as u32, false, None)
                {
                    error!("Failed to register route {} {}: {}", method_str, path, e);
                    return -1; // Error
                }
                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_route: {}", e)))?;

    // _http_route_protected - Register a protected route requiring authentication
    linker
        .func_wrap(
            "env",
            "_http_route_protected",
            |mut caller: Caller<'_, WasmState>,
             method_ptr: i32,
             method_len: i32,
             path_ptr: i32,
             path_len: i32,
             handler_idx: i32,
             role_ptr: i32,
             role_len: i32|
             -> i32 {
                let method_str = read_raw_string(&mut caller, method_ptr, method_len)
                    .unwrap_or_else(|| "GET".to_string());
                let path = read_raw_string(&mut caller, path_ptr, path_len)
                    .unwrap_or_else(|| "/".to_string());
                let required_role = if role_len > 0 {
                    read_raw_string(&mut caller, role_ptr, role_len)
                } else {
                    None
                };

                debug!(
                    "_http_route_protected: method={}, path={}, handler={}, role={:?}",
                    method_str, path, handler_idx, required_role
                );

                let method = match HttpMethod::from_str(&method_str) {
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
                    handler_idx as u32,
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
                    None => return write_string_to_caller(&mut caller, ""),
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

    Ok(())
}

/// Register session management functions (_session_create, _session_get, _session_destroy)
fn register_session_management_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
    // _session_create - Create a new session for a user
    // Arguments: user_id (i32), role_ptr, role_len, claims_ptr, claims_len
    // Returns: session_id string pointer
    linker
        .func_wrap(
            "env",
            "_session_create",
            |mut caller: Caller<'_, WasmState>,
             user_id: i32,
             role_ptr: i32,
             role_len: i32,
             claims_ptr: i32,
             claims_len: i32|
             -> i32 {
                let role = read_raw_string(&mut caller, role_ptr, role_len)
                    .unwrap_or_else(|| "user".to_string());
                let claims = read_raw_string(&mut caller, claims_ptr, claims_len)
                    .unwrap_or_else(|| "{}".to_string());

                info!("_session_create: user_id={}, role={}", user_id, role);

                // Get the session store and create session (uses std::sync::RwLock)
                let session_store = caller.data().session_store.clone();

                let session = {
                    let mut store = session_store.write().unwrap();
                    store.create(user_id, &role, &claims)
                };

                let session_id = session.session_id.clone();

                // Format and store Set-Cookie header
                let set_cookie = {
                    let store = session_store.read().unwrap();
                    store.format_cookie(&session_id)
                };

                caller.data_mut().pending_set_cookie = Some(set_cookie);

                // Set auth context for current request
                caller.data_mut().set_auth_from_session(user_id, role, session_id.clone());

                write_string_to_caller(&mut caller, &session_id)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_create: {}", e)))?;

    // _session_get - Get session data by session ID (returns JSON or empty)
    linker
        .func_wrap(
            "env",
            "_session_get",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                // Get session ID from cookie
                let session_id = {
                    let state = caller.data();
                    state
                        .request_context
                        .as_ref()
                        .and_then(|ctx| {
                            ctx.headers
                                .iter()
                                .find(|(k, _)| k.to_lowercase() == "cookie")
                                .and_then(|(_, cookie_header)| {
                                    let cookies = parse_cookies(cookie_header);
                                    // Try common session cookie names
                                    cookies.get("session").cloned()
                                        .or_else(|| cookies.get("todo.sid").cloned())
                                        .or_else(|| cookies.get("sid").cloned())
                                })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_session_get: No session cookie found");
                        return write_string_to_caller(&mut caller, "");
                    }
                };

                debug!("_session_get: Looking up session {}", session_id);

                let session_store = caller.data().session_store.clone();

                let session_data = {
                    let mut store = session_store.write().unwrap();
                    store.get(&session_id)
                };

                match session_data {
                    Some(session) => {
                        // Set auth context
                        caller.data_mut().set_auth_from_session(
                            session.user_id,
                            session.role.clone(),
                            session.session_id.clone(),
                        );

                        let json = serde_json::json!({
                            "userId": session.user_id,
                            "role": session.role,
                            "sessionId": session.session_id,
                            "claims": session.claims
                        })
                        .to_string();
                        write_string_to_caller(&mut caller, &json)
                    }
                    None => {
                        debug!("_session_get: Session {} not found or expired", session_id);
                        write_string_to_caller(&mut caller, "")
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_get: {}", e)))?;

    // _session_destroy - Destroy the current session (logout)
    linker
        .func_wrap(
            "env",
            "_session_destroy",
            |mut caller: Caller<'_, WasmState>| -> i32 {
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
                                            .or_else(|| cookies.get("todo.sid").cloned())
                                            .or_else(|| cookies.get("sid").cloned())
                                    })
                            })
                        })
                };

                let session_id = match session_id {
                    Some(id) => id,
                    None => {
                        debug!("_session_destroy: No session to destroy");
                        return 0;
                    }
                };

                info!("_session_destroy: Destroying session {}", session_id);

                let session_store = caller.data().session_store.clone();

                // Delete session
                let deleted = {
                    let mut store = session_store.write().unwrap();
                    store.delete(&session_id)
                };

                // Set clear cookie header
                let clear_cookie = {
                    let store = session_store.read().unwrap();
                    store.format_clear_cookie()
                };

                caller.data_mut().pending_set_cookie = Some(clear_cookie);
                caller.data_mut().auth_context = None;

                if deleted { 1 } else { 0 }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_destroy: {}", e)))?;

    // _session_set_cookie - Set a pending Set-Cookie header (for custom cookie values)
    linker
        .func_wrap(
            "env",
            "_session_set_cookie",
            |mut caller: Caller<'_, WasmState>, cookie_ptr: i32, cookie_len: i32| -> i32 {
                let cookie = match read_raw_string(&mut caller, cookie_ptr, cookie_len) {
                    Some(s) => s,
                    None => return 0,
                };

                debug!("_session_set_cookie: {}", cookie);
                caller.data_mut().pending_set_cookie = Some(cookie);
                1
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _session_set_cookie: {}", e)))?;

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
                let permission = match read_raw_string(&mut caller, permission_ptr, permission_len)
                {
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

    Ok(())
}

/// Register response manipulation functions (_res_set_header, _res_redirect)
fn register_response_functions(linker: &mut Linker<WasmState>) -> RuntimeResult<()> {
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
}
