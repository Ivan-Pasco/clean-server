//! Host Bridge WASM Imports
//!
//! Provides host functions that WASM modules expect:
//!
//! ## Bridge Functions (platform services)
//! - I/O functions (print, input) - bridge:io
//! - Memory management (mem_alloc, mem_retain, mem_release)
//! - HTTP server functions (_http_listen, _http_route) - bridge:server
//! - HTTP client functions (http_get, http_post, etc.) - bridge:http
//! - File I/O (file_read, file_write, etc.) - bridge:fs
//! - Database functions (_db_query, _db_execute) - bridge:db
//! - Authentication functions (_auth_verify, _auth_create_session) - bridge:auth
//! - Math functions (sin, cos, tan, pow, ln, exp, etc.) - transcendental operations
//!
//! ## Stdlib Functions (still imported, may become native)
//! - float_to_string, string_to_float - float conversions
//! - string.concat, string.split - string operations
//!
//! ## Now Native WASM (v0.17.1+)
//! - int_to_string, bool_to_string, string_to_int

use crate::error::{RuntimeError, RuntimeResult};
use crate::memory::{read_string_from_caller, STRING_LENGTH_PREFIX_SIZE};
use crate::router::HttpMethod;
use crate::wasm::WasmState;
use tracing::{debug, error, info};
use wasmtime::{Caller, Engine, Linker};

/// Create a linker with all host functions
pub fn create_linker(engine: &Engine) -> RuntimeResult<Linker<WasmState>> {
    let mut linker = Linker::new(engine);

    // =========================================
    // ENV NAMESPACE - Core I/O
    // =========================================

    // print - Print without newline
    linker
        .func_wrap(
            "env",
            "print",
            |mut caller: Caller<'_, WasmState>, ptr: i32, len: i32| {
                if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                    let data = memory.data(&caller);
                    if let Some(slice) = data.get(ptr as usize..(ptr as usize + len as usize)) {
                        if let Ok(s) = std::str::from_utf8(slice) {
                            print!("{}", s);
                        }
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define print: {}", e)))?;

    // printl - Print with newline
    linker
        .func_wrap(
            "env",
            "printl",
            |mut caller: Caller<'_, WasmState>, ptr: i32, len: i32| {
                if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                    let data = memory.data(&caller);
                    if let Some(slice) = data.get(ptr as usize..(ptr as usize + len as usize)) {
                        if let Ok(s) = std::str::from_utf8(slice) {
                            println!("{}", s);
                        }
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define printl: {}", e)))?;

    // input - Read user input (returns empty in server context)
    linker
        .func_wrap(
            "env",
            "input",
            |mut caller: Caller<'_, WasmState>, _prompt_ptr: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(8) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define input: {}", e)))?;

    // input_integer
    linker
        .func_wrap(
            "env",
            "input_integer",
            |_: Caller<'_, WasmState>, _prompt_ptr: i32| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define input_integer: {}", e)))?;

    // input_float
    linker
        .func_wrap(
            "env",
            "input_float",
            |_: Caller<'_, WasmState>, _prompt_ptr: i32| -> f64 { 0.0 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define input_float: {}", e)))?;

    // input_yesno
    linker
        .func_wrap(
            "env",
            "input_yesno",
            |_: Caller<'_, WasmState>, _prompt_ptr: i32| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define input_yesno: {}", e)))?;

    // input_range
    linker
        .func_wrap(
            "env",
            "input_range",
            |_: Caller<'_, WasmState>, _prompt_ptr: i32, min: i32, _max: i32, _default: i32| -> i32 {
                min
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define input_range: {}", e)))?;

    // =========================================
    // STDLIB: Type Conversion Functions (still imported by compiler)
    // Note: int_to_string, bool_to_string, string_to_int are now native WASM (v0.17.1+)
    // =========================================

    // float_to_string - still imported by compiler
    linker
        .func_wrap(
            "env",
            "float_to_string",
            |mut caller: Caller<'_, WasmState>, value: f64| -> i32 {
                let s = value.to_string();
                write_string_to_caller(&mut caller, &s)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define float_to_string: {}", e)))?;

    // string_to_float - still imported by compiler
    linker
        .func_wrap(
            "env",
            "string_to_float",
            |mut caller: Caller<'_, WasmState>, str_ptr: i32| -> f64 {
                if let Ok(s) = read_string_from_caller(&mut caller, str_ptr as u32) {
                    s.parse::<f64>().unwrap_or(0.0)
                } else {
                    0.0
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define string_to_float: {}", e)))?;

    // =========================================
    // STDLIB: String Operations (temporary - until compiler enables native stdlib)
    // =========================================

    // string_concat - Concatenate two strings
    linker
        .func_wrap(
            "env",
            "string_concat",
            |mut caller: Caller<'_, WasmState>,
             str1_ptr: i32,
             str1_len: i32,
             str2_ptr: i32,
             str2_len: i32|
             -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 0,
                };

                let data = memory.data(&caller);

                // Read first string
                let s1_start = str1_ptr as usize;
                let s1_end = s1_start + str1_len as usize;
                let s1 = if s1_end <= data.len() {
                    data[s1_start..s1_end].to_vec()
                } else {
                    Vec::new()
                };

                // Read second string
                let s2_start = str2_ptr as usize;
                let s2_end = s2_start + str2_len as usize;
                let s2 = if s2_end <= data.len() {
                    data[s2_start..s2_end].to_vec()
                } else {
                    Vec::new()
                };

                // Concatenate
                let mut result = s1;
                result.extend(s2);

                // Write result
                write_bytes_to_caller(&mut caller, &result)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define string_concat: {}", e)))?;

    // string.concat - Alias for string_concat (matches compiler import naming)
    linker
        .func_wrap(
            "env",
            "string.concat",
            |mut caller: Caller<'_, WasmState>,
             str1_ptr: i32,
             str1_len: i32,
             str2_ptr: i32,
             str2_len: i32|
             -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 0,
                };

                let data = memory.data(&caller);

                // Read first string
                let s1_start = str1_ptr as usize;
                let s1_end = s1_start + str1_len as usize;
                let s1 = if s1_end <= data.len() {
                    data[s1_start..s1_end].to_vec()
                } else {
                    Vec::new()
                };

                // Read second string
                let s2_start = str2_ptr as usize;
                let s2_end = s2_start + str2_len as usize;
                let s2 = if s2_end <= data.len() {
                    data[s2_start..s2_end].to_vec()
                } else {
                    Vec::new()
                };

                // Concatenate
                let mut result = s1;
                result.extend(s2);

                // Write result
                write_bytes_to_caller(&mut caller, &result)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define string.concat: {}", e)))?;

    // string.split
    linker
        .func_wrap(
            "env",
            "string.split",
            |mut caller: Caller<'_, WasmState>, _str_ptr: i32, _delim_ptr: i32| -> i32 {
                // Return empty array
                let state = caller.data_mut();
                let ptr = state.memory.allocate(4);
                if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                    let _ = memory.write(&mut caller, ptr, &[0u8; 4]);
                }
                ptr as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define string.split: {}", e)))?;

    // =========================================
    // MATH FUNCTIONS (imported by compiler for transcendental operations)
    // =========================================

    // math_pow - power function (x^y)
    linker
        .func_wrap("env", "math_pow", |_: Caller<'_, WasmState>, base: f64, exp: f64| -> f64 {
            base.powf(exp)
        })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_pow: {}", e)))?;

    // Trigonometric functions
    linker
        .func_wrap("env", "math_sin", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.sin() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_sin: {}", e)))?;

    linker
        .func_wrap("env", "math_cos", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.cos() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_cos: {}", e)))?;

    linker
        .func_wrap("env", "math.cos", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.cos() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.cos: {}", e)))?;

    linker
        .func_wrap("env", "math_tan", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.tan() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_tan: {}", e)))?;

    // Inverse trigonometric functions
    linker
        .func_wrap("env", "math_asin", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.asin() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_asin: {}", e)))?;

    linker
        .func_wrap("env", "math_acos", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.acos() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_acos: {}", e)))?;

    linker
        .func_wrap("env", "math.acos", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.acos() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.acos: {}", e)))?;

    linker
        .func_wrap("env", "math_atan", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.atan() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_atan: {}", e)))?;

    linker
        .func_wrap("env", "math_atan2", |_: Caller<'_, WasmState>, y: f64, x: f64| -> f64 { y.atan2(x) })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_atan2: {}", e)))?;

    linker
        .func_wrap("env", "math.atan2", |_: Caller<'_, WasmState>, y: f64, x: f64| -> f64 { y.atan2(x) })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.atan2: {}", e)))?;

    // Hyperbolic functions
    linker
        .func_wrap("env", "math_sinh", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.sinh() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_sinh: {}", e)))?;

    linker
        .func_wrap("env", "math.sinh", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.sinh() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.sinh: {}", e)))?;

    linker
        .func_wrap("env", "math_cosh", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.cosh() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_cosh: {}", e)))?;

    linker
        .func_wrap("env", "math.cosh", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.cosh() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.cosh: {}", e)))?;

    linker
        .func_wrap("env", "math_tanh", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.tanh() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_tanh: {}", e)))?;

    linker
        .func_wrap("env", "math.tanh", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.tanh() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.tanh: {}", e)))?;

    // Logarithmic functions
    linker
        .func_wrap("env", "math_ln", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.ln() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_ln: {}", e)))?;

    linker
        .func_wrap("env", "math.ln", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.ln() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.ln: {}", e)))?;

    linker
        .func_wrap("env", "math_log10", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.log10() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_log10: {}", e)))?;

    linker
        .func_wrap("env", "math_log2", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.log2() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_log2: {}", e)))?;

    // Exponential functions
    linker
        .func_wrap("env", "math_exp", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.exp() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_exp: {}", e)))?;

    linker
        .func_wrap("env", "math.exp", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.exp() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.exp: {}", e)))?;

    linker
        .func_wrap("env", "math_exp2", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.exp2() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math_exp2: {}", e)))?;

    linker
        .func_wrap("env", "math.exp2", |_: Caller<'_, WasmState>, x: f64| -> f64 { x.exp2() })
        .map_err(|e| RuntimeError::wasm(format!("Failed to define math.exp2: {}", e)))?;

    // =========================================
    // MEMORY_RUNTIME NAMESPACE
    // =========================================

    // mem_alloc
    linker
        .func_wrap(
            "memory_runtime",
            "mem_alloc",
            |mut caller: Caller<'_, WasmState>, size: i32, _align: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(size as usize) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define mem_alloc: {}", e)))?;

    // mem_retain (no-op for now)
    linker
        .func_wrap(
            "memory_runtime",
            "mem_retain",
            |_: Caller<'_, WasmState>, _ptr: i32| {},
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define mem_retain: {}", e)))?;

    // mem_release (no-op for now)
    linker
        .func_wrap(
            "memory_runtime",
            "mem_release",
            |_: Caller<'_, WasmState>, _ptr: i32| {},
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define mem_release: {}", e)))?;

    // =========================================
    // HTTP SERVER FUNCTIONS (Frame-specific)
    // =========================================

    // _http_listen - Start listening on a port
    linker
        .func_wrap(
            "env",
            "_http_listen",
            |mut caller: Caller<'_, WasmState>, port: i32| -> i32 {
                info!("WASM requested HTTP listen on port {}", port);
                caller.data_mut().port = port as u16;
                0 // Success
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
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let data = memory.data(&caller);

                // Read method
                let method_str = read_raw_string(data, method_ptr, method_len);
                let path_str = read_raw_string(data, path_ptr, path_len);

                info!(
                    "Registering route: {} {} -> handler {}",
                    method_str, path_str, handler_idx
                );

                let method = match HttpMethod::from_str(&method_str) {
                    Ok(m) => m,
                    Err(_) => {
                        error!("Invalid HTTP method: {}", method_str);
                        return -1;
                    }
                };

                if let Err(e) = caller.data().router.register(
                    method,
                    path_str,
                    handler_idx as u32,
                    false,
                    None,
                ) {
                    error!("Failed to register route: {}", e);
                    return -1;
                }

                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_route: {}", e)))?;

    // _http_route_protected - Register a protected route
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
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let data = memory.data(&caller);

                let method_str = read_raw_string(data, method_ptr, method_len);
                let path_str = read_raw_string(data, path_ptr, path_len);
                let role_str = if role_len > 0 {
                    Some(read_raw_string(data, role_ptr, role_len))
                } else {
                    None
                };

                info!(
                    "Registering protected route: {} {} -> handler {} (role: {:?})",
                    method_str, path_str, handler_idx, role_str
                );

                let method = match HttpMethod::from_str(&method_str) {
                    Ok(m) => m,
                    Err(_) => return -1,
                };

                if let Err(_) = caller.data().router.register(
                    method,
                    path_str,
                    handler_idx as u32,
                    true,
                    role_str,
                ) {
                    return -1;
                }

                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _http_route_protected: {}", e)))?;

    // =========================================
    // REQUEST CONTEXT ACCESS FUNCTIONS
    // =========================================

    // _req_param - Get a path parameter by name (e.g., :id from /users/:id)
    linker
        .func_wrap(
            "env",
            "_req_param",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                debug!("_req_param called: ptr={}, len={}", name_ptr, name_len);

                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => {
                        error!("_req_param: No memory export found");
                        return 0;
                    }
                };

                let data = memory.data(&caller);
                let param_name = read_raw_string(data, name_ptr, name_len);
                debug!("_req_param: Looking for param '{}'", param_name);

                // Get the request context from state
                let value = {
                    let state = caller.data();
                    if let Some(ref ctx) = state.request_context {
                        debug!("_req_param: Request context has {} params", ctx.params.len());
                        for (k, v) in &ctx.params {
                            debug!("_req_param: param '{}' = '{}'", k, v);
                        }
                        ctx.params.get(&param_name).cloned()
                    } else {
                        debug!("_req_param: No request context!");
                        None
                    }
                };

                // Write the parameter value to WASM memory
                match value {
                    Some(v) => {
                        debug!("_req_param: Found value '{}', writing to memory", v);
                        let ptr = write_string_to_caller(&mut caller, &v);
                        debug!("_req_param: Wrote to ptr {}", ptr);
                        ptr
                    }
                    None => {
                        debug!("_req_param: No value found for '{}', writing empty string", param_name);
                        write_string_to_caller(&mut caller, "")
                    }
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_param: {}", e)))?;

    // _req_query - Get a query parameter by name (e.g., ?q=search)
    linker
        .func_wrap(
            "env",
            "_req_query",
            |mut caller: Caller<'_, WasmState>, name_ptr: i32, name_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 0,
                };

                let data = memory.data(&caller);
                let param_name = read_raw_string(data, name_ptr, name_len);

                // Get the request context from state
                let value = {
                    let state = caller.data();
                    if let Some(ref ctx) = state.request_context {
                        ctx.query.get(&param_name).cloned()
                    } else {
                        None
                    }
                };

                match value {
                    Some(v) => write_string_to_caller(&mut caller, &v),
                    None => write_string_to_caller(&mut caller, ""),
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_query: {}", e)))?;

    // _req_body - Get the request body as a string
    linker
        .func_wrap(
            "env",
            "_req_body",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let body = {
                    let state = caller.data();
                    if let Some(ref ctx) = state.request_context {
                        ctx.body.clone()
                    } else {
                        String::new()
                    }
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
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 0,
                };

                let data = memory.data(&caller);
                let header_name = read_raw_string(data, name_ptr, name_len).to_lowercase();

                // Get the request context from state
                let value = {
                    let state = caller.data();
                    if let Some(ref ctx) = state.request_context {
                        ctx.headers
                            .iter()
                            .find(|(name, _)| name.to_lowercase() == header_name)
                            .map(|(_, v)| v.clone())
                    } else {
                        None
                    }
                };

                match value {
                    Some(v) => write_string_to_caller(&mut caller, &v),
                    None => write_string_to_caller(&mut caller, ""),
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_header: {}", e)))?;

    // _req_method - Get the request method
    linker
        .func_wrap(
            "env",
            "_req_method",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let method = {
                    let state = caller.data();
                    if let Some(ref ctx) = state.request_context {
                        ctx.method.clone()
                    } else {
                        String::new()
                    }
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
                    if let Some(ref ctx) = state.request_context {
                        ctx.path.clone()
                    } else {
                        String::new()
                    }
                };
                write_string_to_caller(&mut caller, &path)
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _req_path: {}", e)))?;

    // =========================================
    // HTTP CLIENT FUNCTIONS
    // =========================================

    // http_get
    linker
        .func_wrap(
            "env",
            "http_get",
            |mut caller: Caller<'_, WasmState>, _url_ptr: i32, _url_len: i32| -> i32 {
                // Return empty response placeholder
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_get: {}", e)))?;

    // http_post
    linker
        .func_wrap(
            "env",
            "http_post",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _body_ptr: i32,
             _body_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_post: {}", e)))?;

    // http_put
    linker
        .func_wrap(
            "env",
            "http_put",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _body_ptr: i32,
             _body_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_put: {}", e)))?;

    // http_patch
    linker
        .func_wrap(
            "env",
            "http_patch",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _body_ptr: i32,
             _body_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_patch: {}", e)))?;

    // http_delete
    linker
        .func_wrap(
            "env",
            "http_delete",
            |mut caller: Caller<'_, WasmState>, _url_ptr: i32, _url_len: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_delete: {}", e)))?;

    // http_head
    linker
        .func_wrap(
            "env",
            "http_head",
            |mut caller: Caller<'_, WasmState>, _url_ptr: i32, _url_len: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_head: {}", e)))?;

    // http_options
    linker
        .func_wrap(
            "env",
            "http_options",
            |mut caller: Caller<'_, WasmState>, _url_ptr: i32, _url_len: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_options: {}", e)))?;

    // http_get_with_headers
    linker
        .func_wrap(
            "env",
            "http_get_with_headers",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _headers_ptr: i32,
             _headers_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_get_with_headers: {}", e)))?;

    // http_post_with_headers
    linker
        .func_wrap(
            "env",
            "http_post_with_headers",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _body_ptr: i32,
             _body_len: i32,
             _headers_ptr: i32,
             _headers_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_post_with_headers: {}", e)))?;

    // http_post_json
    linker
        .func_wrap(
            "env",
            "http_post_json",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _json_ptr: i32,
             _json_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_post_json: {}", e)))?;

    // http_put_json
    linker
        .func_wrap(
            "env",
            "http_put_json",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _json_ptr: i32,
             _json_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_put_json: {}", e)))?;

    // http_patch_json
    linker
        .func_wrap(
            "env",
            "http_patch_json",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _json_ptr: i32,
             _json_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_patch_json: {}", e)))?;

    // http_post_form
    linker
        .func_wrap(
            "env",
            "http_post_form",
            |mut caller: Caller<'_, WasmState>,
             _url_ptr: i32,
             _url_len: i32,
             _form_ptr: i32,
             _form_len: i32|
             -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_post_form: {}", e)))?;

    // http_set_user_agent
    linker
        .func_wrap(
            "env",
            "http_set_user_agent",
            |_: Caller<'_, WasmState>, _ua_ptr: i32, _ua_len: i32| {},
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_set_user_agent: {}", e)))?;

    // http_set_timeout
    linker
        .func_wrap(
            "env",
            "http_set_timeout",
            |_: Caller<'_, WasmState>, _timeout_ms: i32| {},
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_set_timeout: {}", e)))?;

    // http_set_max_redirects
    linker
        .func_wrap(
            "env",
            "http_set_max_redirects",
            |_: Caller<'_, WasmState>, _max: i32| {},
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_set_max_redirects: {}", e)))?;

    // http_enable_cookies
    linker
        .func_wrap(
            "env",
            "http_enable_cookies",
            |_: Caller<'_, WasmState>, _enable: i32| {},
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_enable_cookies: {}", e)))?;

    // http_get_response_code
    linker
        .func_wrap(
            "env",
            "http_get_response_code",
            |_: Caller<'_, WasmState>| -> i32 { 200 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_get_response_code: {}", e)))?;

    // http_get_response_headers
    linker
        .func_wrap(
            "env",
            "http_get_response_headers",
            |_: Caller<'_, WasmState>| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_get_response_headers: {}", e)))?;

    // http_encode_url
    linker
        .func_wrap(
            "env",
            "http_encode_url",
            |_: Caller<'_, WasmState>, url_ptr: i32, _url_len: i32| -> i32 { url_ptr },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_encode_url: {}", e)))?;

    // http_decode_url
    linker
        .func_wrap(
            "env",
            "http_decode_url",
            |_: Caller<'_, WasmState>, url_ptr: i32, _url_len: i32| -> i32 { url_ptr },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_decode_url: {}", e)))?;

    // http_build_query
    linker
        .func_wrap(
            "env",
            "http_build_query",
            |mut caller: Caller<'_, WasmState>, _params_ptr: i32, _params_len: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define http_build_query: {}", e)))?;

    // =========================================
    // FILE I/O FUNCTIONS
    // =========================================

    // file_write
    linker
        .func_wrap(
            "env",
            "file_write",
            |_: Caller<'_, WasmState>,
             _path_ptr: i32,
             _path_len: i32,
             _content_ptr: i32,
             _content_len: i32|
             -> i32 {
                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define file_write: {}", e)))?;

    // file_read
    linker
        .func_wrap(
            "env",
            "file_read",
            |mut caller: Caller<'_, WasmState>, _path_ptr: i32, _path_len: i32, _buf_ptr: i32| -> i32 {
                let state = caller.data_mut();
                state.memory.allocate(4) as i32
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define file_read: {}", e)))?;

    // file_exists
    linker
        .func_wrap(
            "env",
            "file_exists",
            |_: Caller<'_, WasmState>, _path_ptr: i32, _path_len: i32| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define file_exists: {}", e)))?;

    // file_delete
    linker
        .func_wrap(
            "env",
            "file_delete",
            |_: Caller<'_, WasmState>, _path_ptr: i32, _path_len: i32| -> i32 { 0 },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define file_delete: {}", e)))?;

    // file_append
    linker
        .func_wrap(
            "env",
            "file_append",
            |_: Caller<'_, WasmState>,
             _path_ptr: i32,
             _path_len: i32,
             _content_ptr: i32,
             _content_len: i32|
             -> i32 {
                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define file_append: {}", e)))?;

    // =========================================
    // DATABASE FUNCTIONS
    // =========================================

    // _db_query - Execute a SELECT query
    linker
        .func_wrap(
            "env",
            "_db_query",
            |mut caller: Caller<'_, WasmState>,
             _sql_ptr: i32,
             _sql_len: i32,
             _params_ptr: i32,
             _params_len: i32|
             -> i32 {
                // Return empty result set
                let state = caller.data_mut();
                write_string_to_caller_state(state, "[]")
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _db_query: {}", e)))?;

    // _db_execute - Execute an INSERT/UPDATE/DELETE
    linker
        .func_wrap(
            "env",
            "_db_execute",
            |_: Caller<'_, WasmState>,
             _sql_ptr: i32,
             _sql_len: i32,
             _params_ptr: i32,
             _params_len: i32|
             -> i32 {
                0 // Rows affected
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _db_execute: {}", e)))?;

    // =========================================
    // AUTH FUNCTIONS
    // =========================================

    // _auth_verify - Verify a token/session
    linker
        .func_wrap(
            "env",
            "_auth_verify",
            |_: Caller<'_, WasmState>, _token_ptr: i32, _token_len: i32| -> i32 {
                0 // Not verified
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_verify: {}", e)))?;

    // _auth_create_session - Create a new session
    linker
        .func_wrap(
            "env",
            "_auth_create_session",
            |mut caller: Caller<'_, WasmState>, _user_id: i32| -> i32 {
                let state = caller.data_mut();
                write_string_to_caller_state(state, "session_id")
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_create_session: {}", e)))?;

    // _auth_destroy_session - Destroy a session
    linker
        .func_wrap(
            "env",
            "_auth_destroy_session",
            |_: Caller<'_, WasmState>, _session_ptr: i32, _session_len: i32| -> i32 {
                0 // Success
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_destroy_session: {}", e)))?;

    // _auth_hash_password - Hash a password
    linker
        .func_wrap(
            "env",
            "_auth_hash_password",
            |mut caller: Caller<'_, WasmState>, _password_ptr: i32, _password_len: i32| -> i32 {
                let state = caller.data_mut();
                write_string_to_caller_state(state, "$argon2id$hash")
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_hash_password: {}", e)))?;

    // _auth_verify_password - Verify a password against hash
    linker
        .func_wrap(
            "env",
            "_auth_verify_password",
            |_: Caller<'_, WasmState>,
             _password_ptr: i32,
             _password_len: i32,
             _hash_ptr: i32,
             _hash_len: i32|
             -> i32 {
                0 // Not verified
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_verify_password: {}", e)))?;

    // =========================================
    // AUTH GUARD FUNCTIONS (for route protection)
    // =========================================

    // _auth_get_session - Get the current session/auth context
    linker
        .func_wrap(
            "env",
            "_auth_get_session",
            |mut caller: Caller<'_, WasmState>| -> i32 {
                let has_auth = {
                    let state = caller.data();
                    state.auth_context.is_some()
                };

                if has_auth {
                    // Return session info as JSON
                    let session_json = {
                        let state = caller.data();
                        if let Some(ref auth) = state.auth_context {
                            format!(
                                "{{\"user_id\":{},\"role\":\"{}\",\"session_id\":\"{}\"}}",
                                auth.user_id,
                                auth.role,
                                auth.session_id.as_deref().unwrap_or("")
                            )
                        } else {
                            "null".to_string()
                        }
                    };
                    write_string_to_caller(&mut caller, &session_json)
                } else {
                    write_string_to_caller(&mut caller, "null")
                }
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_get_session: {}", e)))?;

    // _auth_require_auth - Check if user is authenticated
    // Returns 1 if authenticated, 0 if not
    linker
        .func_wrap(
            "env",
            "_auth_require_auth",
            |caller: Caller<'_, WasmState>| -> i32 {
                let state = caller.data();

                // Check if auth context exists
                if state.auth_context.is_some() {
                    debug!("Auth check passed: user is authenticated");
                    return 1; // Authenticated
                }

                // Check for authorization header in request context
                if let Some(ref ctx) = state.request_context {
                    for (name, _value) in &ctx.headers {
                        if name.to_lowercase() == "authorization" {
                            // For now, accept any authorization header as authenticated
                            // In production, this would validate the token/session
                            debug!("Auth check passed: Authorization header present");
                            return 1;
                        }
                    }
                }

                debug!("Auth check failed: no authentication found");
                0 // Not authenticated
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_require_auth: {}", e)))?;

    // _auth_require_role - Check if user has a specific role
    // Returns 1 if user has role, 0 if not
    linker
        .func_wrap(
            "env",
            "_auth_require_role",
            |mut caller: Caller<'_, WasmState>, role_ptr: i32, role_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 0,
                };

                let data = memory.data(&caller);
                let required_role = read_raw_string(data, role_ptr, role_len);

                debug!("Checking for role: {}", required_role);

                let state = caller.data();

                // Check auth context for role
                if let Some(ref auth) = state.auth_context {
                    if auth.role == required_role || auth.role == "admin" {
                        // Admin role has access to everything
                        debug!("Role check passed: user has role '{}' or 'admin'", required_role);
                        return 1;
                    }
                }

                // For testing: check for X-User-Role header
                if let Some(ref ctx) = state.request_context {
                    for (name, value) in &ctx.headers {
                        if name.to_lowercase() == "x-user-role" {
                            if value == &required_role || value == "admin" {
                                debug!("Role check passed: X-User-Role header matches");
                                return 1;
                            }
                        }
                    }
                }

                debug!("Role check failed: user does not have required role '{}'", required_role);
                0 // Does not have role
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_require_role: {}", e)))?;

    // _auth_can - Check if user has a specific permission
    // Returns 1 if user has permission, 0 if not
    linker
        .func_wrap(
            "env",
            "_auth_can",
            |mut caller: Caller<'_, WasmState>, perm_ptr: i32, perm_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return 0,
                };

                let data = memory.data(&caller);
                let required_perm = read_raw_string(data, perm_ptr, perm_len);

                debug!("Checking for permission: {}", required_perm);

                let state = caller.data();

                // Admin role has all permissions
                if let Some(ref auth) = state.auth_context {
                    if auth.role == "admin" {
                        debug!("Permission check passed: user is admin");
                        return 1;
                    }
                }

                // For testing: check for X-User-Permissions header (comma-separated)
                if let Some(ref ctx) = state.request_context {
                    for (name, value) in &ctx.headers {
                        if name.to_lowercase() == "x-user-permissions" {
                            let perms: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
                            if perms.contains(&required_perm.as_str()) {
                                debug!("Permission check passed: user has permission");
                                return 1;
                            }
                        }
                    }
                }

                debug!("Permission check failed: user does not have permission '{}'", required_perm);
                0 // Does not have permission
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_can: {}", e)))?;

    // _auth_has_any_role - Check if user has any role (generic check)
    // Returns 1 if user is authenticated with any role, 0 if not
    linker
        .func_wrap(
            "env",
            "_auth_has_any_role",
            |caller: Caller<'_, WasmState>| -> i32 {
                let state = caller.data();

                if let Some(ref auth) = state.auth_context {
                    if !auth.role.is_empty() {
                        debug!("Has role check passed: user has role '{}'", auth.role);
                        return 1;
                    }
                }

                // Check for X-User-Role header
                if let Some(ref ctx) = state.request_context {
                    for (name, value) in &ctx.headers {
                        if name.to_lowercase() == "x-user-role" && !value.is_empty() {
                            debug!("Has role check passed: X-User-Role header present");
                            return 1;
                        }
                    }
                }

                debug!("Has role check failed: no role found");
                0
            },
        )
        .map_err(|e| RuntimeError::wasm(format!("Failed to define _auth_has_any_role: {}", e)))?;

    info!("Host Bridge linker initialized with all functions");
    Ok(linker)
}

/// Helper to write a string to WASM memory using caller
fn write_string_to_caller(caller: &mut Caller<'_, WasmState>, s: &str) -> i32 {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let total_size = STRING_LENGTH_PREFIX_SIZE + len;

    let state = caller.data_mut();
    let ptr = state.memory.allocate(total_size);

    if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
        // Ensure memory is large enough
        let required = ptr + total_size;
        let current_size = memory.data_size(&*caller);

        if required > current_size {
            // Calculate required pages (64KB per page)
            let required_pages = ((required + 65535) / 65536) as u64;
            let current_pages = memory.size(&*caller);
            let pages_to_grow = required_pages.saturating_sub(current_pages);

            if pages_to_grow > 0 {
                if let Err(e) = memory.grow(&mut *caller, pages_to_grow) {
                    error!("Failed to grow memory by {} pages: {}", pages_to_grow, e);
                    return 0;
                }
            }
        }

        let len_bytes = (len as u32).to_le_bytes();
        let _ = memory.write(&mut *caller, ptr, &len_bytes);
        let _ = memory.write(&mut *caller, ptr + STRING_LENGTH_PREFIX_SIZE, bytes);
    }

    ptr as i32
}

/// Helper to write bytes to WASM memory with length prefix
fn write_bytes_to_caller(caller: &mut Caller<'_, WasmState>, bytes: &[u8]) -> i32 {
    let len = bytes.len();

    let state = caller.data_mut();
    let ptr = state.memory.allocate(STRING_LENGTH_PREFIX_SIZE + len);

    if let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) {
        let len_bytes = (len as u32).to_le_bytes();
        let _ = memory.write(&mut *caller, ptr, &len_bytes);
        let _ = memory.write(&mut *caller, ptr + STRING_LENGTH_PREFIX_SIZE, bytes);
    }

    ptr as i32
}

/// Helper to write a string using only state (for functions that don't have caller access to memory)
fn write_string_to_caller_state(state: &mut WasmState, s: &str) -> i32 {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let ptr = state.memory.allocate(STRING_LENGTH_PREFIX_SIZE + len);
    // Note: This won't actually write to memory - it just allocates
    // The actual write would need to happen with memory access
    ptr as i32
}

/// Read a raw string from WASM memory (no length prefix)
fn read_raw_string(data: &[u8], ptr: i32, len: i32) -> String {
    let start = ptr as usize;
    let end = start + len as usize;

    if end <= data.len() {
        std::str::from_utf8(&data[start..end])
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    }
}
