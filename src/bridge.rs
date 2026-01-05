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
//! - Request context (_req_param, _req_query, _req_body, _req_header, _req_method, _req_path)
//! - Session auth (_auth_get_session, _auth_require_auth, _auth_require_role)

use crate::error::{RuntimeError, RuntimeResult};
use crate::router::HttpMethod;
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
    register_session_auth_functions(&mut linker)?;

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
