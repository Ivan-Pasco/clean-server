//! HTTP Server Host Functions
//!
//! Provides HTTP server operations for WASM modules:
//! - Route registration: _http_listen, _http_route, _http_route_protected
//! - Request context: _req_param, _req_query, _req_body, _req_header, etc.
//! - Response building: _http_respond, _res_set_header, _res_redirect
//! - Authentication: _auth_get_session, _auth_require_auth, _auth_require_role
//!
//! These are server-specific extensions (not portable across all hosts).
//! See platform-architecture/SERVER_EXTENSIONS.md for specification.

use super::helpers::{read_raw_string, write_string_to_caller};
use super::state::WasmStateCore;
use crate::error::BridgeResult;
use serde_json::json;
use tracing::{debug, error, warn};
use wasmtime::{Caller, Linker};

/// Register all HTTP server functions with the linker
pub fn register_functions<S: WasmStateCore>(linker: &mut Linker<S>) -> BridgeResult<()> {
    // =========================================
    // SERVER CONFIGURATION
    // =========================================

    // _http_listen - Configure the HTTP server port
    linker.func_wrap(
        "env",
        "_http_listen",
        |mut caller: Caller<'_, S>, port: i32| -> i32 {
            debug!("_http_listen: Setting port to {}", port);
            let state = caller.data_mut();
            state.set_port(port as u16);
            1 // Success
        },
    )?;

    // _http_route - Register a route handler
    // Args: method_ptr, method_len, path_ptr, path_len, handler_idx
    linker.func_wrap(
        "env",
        "_http_route",
        |mut caller: Caller<'_, S>,
         method_ptr: i32,
         method_len: i32,
         path_ptr: i32,
         path_len: i32,
         handler_idx: i32| {
            let method = read_raw_string(&mut caller, method_ptr, method_len)
                .unwrap_or_default();
            let path = read_raw_string(&mut caller, path_ptr, path_len)
                .unwrap_or_default();

            debug!("_http_route: {} {} -> handler_{}", method, path, handler_idx);

            if let Some(router) = caller.data().router() {
                if let Err(e) = router.register(&method, path.clone(), handler_idx as u32, false, None) {
                    error!("_http_route: Failed to register route: {}", e);
                }
            } else {
                warn!("_http_route: No router configured");
            }
        },
    )?;

    // _http_route_protected - Register a protected route requiring authentication
    linker.func_wrap(
        "env",
        "_http_route_protected",
        |mut caller: Caller<'_, S>,
         method_ptr: i32,
         method_len: i32,
         path_ptr: i32,
         path_len: i32,
         handler_idx: i32,
         role_ptr: i32,
         role_len: i32| {
            let method = read_raw_string(&mut caller, method_ptr, method_len)
                .unwrap_or_default();
            let path = read_raw_string(&mut caller, path_ptr, path_len)
                .unwrap_or_default();
            let role = if role_len > 0 {
                read_raw_string(&mut caller, role_ptr, role_len)
            } else {
                None
            };

            debug!(
                "_http_route_protected: {} {} -> handler_{} (role: {:?})",
                method, path, handler_idx, role
            );

            if let Some(router) = caller.data().router() {
                if let Err(e) = router.register(&method, path.clone(), handler_idx as u32, true, role) {
                    error!("_http_route_protected: Failed to register route: {}", e);
                }
            } else {
                warn!("_http_route_protected: No router configured");
            }
        },
    )?;

    // =========================================
    // REQUEST CONTEXT - PATH PARAMETERS
    // =========================================

    // _req_param - Get a path parameter by name
    linker.func_wrap(
        "env",
        "_req_param",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let value = caller
                .data()
                .request_context()
                .and_then(|ctx| ctx.params.get(&name))
                .cloned()
                .unwrap_or_default();

            debug!("_req_param({}): {}", name, value);
            write_string_to_caller(&mut caller, &value)
        },
    )?;

    // _req_param_int - Get a path parameter as integer
    linker.func_wrap(
        "env",
        "_req_param_int",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return 0,
            };

            let value = caller
                .data()
                .request_context()
                .and_then(|ctx| ctx.params.get(&name))
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0);

            debug!("_req_param_int({}): {}", name, value);
            value
        },
    )?;

    // =========================================
    // REQUEST CONTEXT - QUERY PARAMETERS
    // =========================================

    // _req_query - Get a query parameter by name
    linker.func_wrap(
        "env",
        "_req_query",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let value = caller
                .data()
                .request_context()
                .and_then(|ctx| ctx.query.get(&name))
                .cloned()
                .unwrap_or_default();

            debug!("_req_query({}): {}", name, value);
            write_string_to_caller(&mut caller, &value)
        },
    )?;

    // =========================================
    // REQUEST CONTEXT - BODY
    // =========================================

    // _req_body - Get the full request body
    linker.func_wrap(
        "env",
        "_req_body",
        |mut caller: Caller<'_, S>| -> i32 {
            // Clone the body to avoid borrow issues
            let body = caller
                .data()
                .request_context()
                .map(|ctx| ctx.body.clone())
                .unwrap_or_default();

            debug!("_req_body: {} bytes", body.len());
            write_string_to_caller(&mut caller, &body)
        },
    )?;

    // _req_body_field - Get a field from JSON request body
    linker.func_wrap(
        "env",
        "_req_body_field",
        |mut caller: Caller<'_, S>, field_ptr: i32, field_len: i32| -> i32 {
            let field_name = match read_raw_string(&mut caller, field_ptr, field_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let value = caller
                .data()
                .request_context()
                .and_then(|ctx| {
                    // Parse body as JSON and extract field
                    serde_json::from_str::<serde_json::Value>(&ctx.body).ok()
                })
                .and_then(|json| {
                    json.get(&field_name).map(|v| {
                        // Return string representation
                        match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        }
                    })
                })
                .unwrap_or_default();

            debug!("_req_body_field({}): {}", field_name, value);
            write_string_to_caller(&mut caller, &value)
        },
    )?;

    // =========================================
    // REQUEST CONTEXT - HEADERS
    // =========================================

    // _req_header - Get a request header by name (case-insensitive)
    linker.func_wrap(
        "env",
        "_req_header",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            let name_lower = name.to_lowercase();
            let value = caller
                .data()
                .request_context()
                .and_then(|ctx| {
                    ctx.headers
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == name_lower)
                        .map(|(_, v)| v.clone())
                })
                .unwrap_or_default();

            debug!("_req_header({}): {}", name, value);
            write_string_to_caller(&mut caller, &value)
        },
    )?;

    // _req_method - Get the HTTP method
    linker.func_wrap(
        "env",
        "_req_method",
        |mut caller: Caller<'_, S>| -> i32 {
            // Clone to avoid borrow issues
            let method = caller
                .data()
                .request_context()
                .map(|ctx| ctx.method.clone())
                .unwrap_or_default();

            write_string_to_caller(&mut caller, &method)
        },
    )?;

    // _req_path - Get the request path
    linker.func_wrap(
        "env",
        "_req_path",
        |mut caller: Caller<'_, S>| -> i32 {
            // Clone to avoid borrow issues
            let path = caller
                .data()
                .request_context()
                .map(|ctx| ctx.path.clone())
                .unwrap_or_default();

            write_string_to_caller(&mut caller, &path)
        },
    )?;

    // _req_cookie - Get a cookie by name
    linker.func_wrap(
        "env",
        "_req_cookie",
        |mut caller: Caller<'_, S>, name_ptr: i32, name_len: i32| -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            // Find Cookie header and parse it
            let value = caller
                .data()
                .request_context()
                .and_then(|ctx| {
                    ctx.headers
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == "cookie")
                        .map(|(_, v)| v.clone())
                })
                .and_then(|cookie_header| {
                    // Parse cookie header: "name1=value1; name2=value2"
                    cookie_header
                        .split(';')
                        .filter_map(|pair| {
                            let mut parts = pair.trim().splitn(2, '=');
                            let key = parts.next()?;
                            let val = parts.next()?;
                            if key == name {
                                Some(val.to_string())
                            } else {
                                None
                            }
                        })
                        .next()
                })
                .unwrap_or_default();

            debug!("_req_cookie({}): {}", name, value);
            write_string_to_caller(&mut caller, &value)
        },
    )?;

    // =========================================
    // RESPONSE BUILDING
    // =========================================

    // _http_respond - Send an HTTP response
    // Args: status, content_type_ptr, content_type_len, body_ptr, body_len
    // Returns: pointer to body (for chaining)
    linker.func_wrap(
        "env",
        "_http_respond",
        |mut caller: Caller<'_, S>,
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

            debug!("_http_respond: status={}, content_type={}, body_len={}",
                   status, content_type, body.len());

            if let Some(response) = caller.data_mut().http_response_mut() {
                response.set_status(status as u16);
                response.set_header("Content-Type".to_string(), content_type);
                response.set_body(body.clone());
            }

            // Return pointer to body for chaining
            write_string_to_caller(&mut caller, &body)
        },
    )?;

    // _res_set_header - Set a response header
    linker.func_wrap(
        "env",
        "_res_set_header",
        |mut caller: Caller<'_, S>,
         name_ptr: i32,
         name_len: i32,
         value_ptr: i32,
         value_len: i32|
         -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return 0,
            };
            let value = read_raw_string(&mut caller, value_ptr, value_len)
                .unwrap_or_default();

            debug!("_res_set_header: {} = {}", name, value);

            if let Some(response) = caller.data_mut().http_response_mut() {
                response.set_header(name, value);
                1 // Success
            } else {
                0 // No response context
            }
        },
    )?;

    // _res_redirect - Send an HTTP redirect
    linker.func_wrap(
        "env",
        "_res_redirect",
        |mut caller: Caller<'_, S>, url_ptr: i32, url_len: i32, status: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return 0,
            };

            debug!("_res_redirect: {} (status={})", url, status);

            if let Some(response) = caller.data_mut().http_response_mut() {
                response.set_redirect(url, status as u16);
                1 // Success
            } else {
                0 // No response context
            }
        },
    )?;

    // _http_redirect - Alias for _res_redirect with different signature
    // Args: status, url_ptr, url_len
    linker.func_wrap(
        "env",
        "_http_redirect",
        |mut caller: Caller<'_, S>, status: i32, url_ptr: i32, url_len: i32| -> i32 {
            let url = match read_raw_string(&mut caller, url_ptr, url_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };

            debug!("_http_redirect: {} (status={})", url, status);

            if let Some(response) = caller.data_mut().http_response_mut() {
                response.set_redirect(url.clone(), status as u16);
            }

            write_string_to_caller(&mut caller, &url)
        },
    )?;

    // _http_set_header - Alias for _res_set_header
    linker.func_wrap(
        "env",
        "_http_set_header",
        |mut caller: Caller<'_, S>,
         name_ptr: i32,
         name_len: i32,
         value_ptr: i32,
         value_len: i32|
         -> i32 {
            let name = match read_raw_string(&mut caller, name_ptr, name_len) {
                Some(s) => s,
                None => return write_string_to_caller(&mut caller, ""),
            };
            let value = read_raw_string(&mut caller, value_ptr, value_len)
                .unwrap_or_default();

            debug!("_http_set_header: {} = {}", name, value);

            if let Some(response) = caller.data_mut().http_response_mut() {
                response.set_header(name.clone(), value);
            }

            write_string_to_caller(&mut caller, &name)
        },
    )?;

    // =========================================
    // AUTHENTICATION
    // =========================================

    // _auth_get_session - Get current session info as JSON
    linker.func_wrap(
        "env",
        "_auth_get_session",
        |mut caller: Caller<'_, S>| -> i32 {
            let session = caller
                .data()
                .auth_context()
                .map(|ctx| {
                    json!({
                        "user_id": ctx.user_id,
                        "role": ctx.role,
                        "session_id": ctx.session_id
                    })
                    .to_string()
                })
                .unwrap_or_else(|| "null".to_string());

            write_string_to_caller(&mut caller, &session)
        },
    )?;

    // _auth_require_auth - Check if current request is authenticated
    linker.func_wrap(
        "env",
        "_auth_require_auth",
        |caller: Caller<'_, S>| -> i32 {
            if caller.data().auth_context().is_some() {
                1
            } else {
                0
            }
        },
    )?;

    // _auth_require_role - Check if user has a specific role
    linker.func_wrap(
        "env",
        "_auth_require_role",
        |mut caller: Caller<'_, S>, role_ptr: i32, role_len: i32| -> i32 {
            let required_role = match read_raw_string(&mut caller, role_ptr, role_len) {
                Some(s) => s,
                None => return 0,
            };

            let has_role = caller
                .data()
                .auth_context()
                .map(|ctx| ctx.role == required_role || ctx.role == "admin")
                .unwrap_or(false);

            if has_role { 1 } else { 0 }
        },
    )?;

    // _auth_can - Check if user has a permission
    linker.func_wrap(
        "env",
        "_auth_can",
        |mut caller: Caller<'_, S>, permission_ptr: i32, permission_len: i32| -> i32 {
            let permission = match read_raw_string(&mut caller, permission_ptr, permission_len) {
                Some(s) => s,
                None => return 0,
            };

            // Admin has all permissions, otherwise check if role matches
            let can = caller
                .data()
                .auth_context()
                .map(|ctx| ctx.role == "admin" || ctx.role == permission)
                .unwrap_or(false);

            if can { 1 } else { 0 }
        },
    )?;

    // _auth_has_any_role - Check if user has any of the specified roles
    linker.func_wrap(
        "env",
        "_auth_has_any_role",
        |mut caller: Caller<'_, S>, roles_ptr: i32, roles_len: i32| -> i32 {
            let roles_json = match read_raw_string(&mut caller, roles_ptr, roles_len) {
                Some(s) => s,
                None => return 0,
            };

            // Parse roles as JSON array
            let roles: Vec<String> = serde_json::from_str(&roles_json).unwrap_or_default();

            let has_role = caller
                .data()
                .auth_context()
                .map(|ctx| {
                    ctx.role == "admin" || roles.contains(&ctx.role)
                })
                .unwrap_or(false);

            if has_role { 1 } else { 0 }
        },
    )?;

    // _auth_user_id - Get current user ID (convenience function)
    linker.func_wrap(
        "env",
        "_auth_user_id",
        |caller: Caller<'_, S>| -> i32 {
            caller
                .data()
                .auth_context()
                .map(|ctx| ctx.user_id)
                .unwrap_or(0)
        },
    )?;

    // _auth_user_role - Get current user role
    linker.func_wrap(
        "env",
        "_auth_user_role",
        |mut caller: Caller<'_, S>| -> i32 {
            // Clone to avoid borrow issues
            let role = caller
                .data()
                .auth_context()
                .map(|ctx| ctx.role.clone())
                .unwrap_or_default();

            write_string_to_caller(&mut caller, &role)
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests require WASM runtime setup
}
