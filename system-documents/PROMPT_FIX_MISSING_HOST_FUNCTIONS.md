# Fix Missing Host Functions in Clean Server

## Problem

When running a Frame application with `cleen frame serve`, the server fails with:

```
WASM error: Failed to instantiate WASM module: unknown import: `env::_req_cookie` has not been defined
```

The Clean Language compiler generates WASM modules that import host functions from the `env` namespace, but the clean-server runtime doesn't define all of them.

## Root Cause

The compiler (clean-language-compiler) generates code that calls host bridge functions like `_req_cookie`, but the clean-server's WASM instantiation doesn't provide these imports.

## Task

Add the missing host functions to clean-server so compiled WASM modules can run.

## Steps to Fix

### 1. Find where host functions are registered

Look in `src/` for where WASM imports are defined. This is typically in the wasmtime/wasmer linker setup where `env::*` functions are registered.

Search for:
- `linker.func_wrap`
- `imports!` macro
- `env` namespace definitions
- Existing host functions like `_print`, `_log`, etc.

### 2. Identify all missing host functions

The compiler may generate calls to these host functions (check clean-language-compiler for the full list):

**Request/Response:**
- `_req_cookie` - Get cookie from request
- `_req_header` - Get header from request
- `_req_body` - Get request body
- `_req_param` - Get URL parameter
- `_req_query` - Get query parameter
- `_res_set_cookie` - Set response cookie
- `_res_set_header` - Set response header
- `_res_redirect` - Send redirect response

**Auth:**
- `_auth_create_session` - Create user session
- `_auth_get_session` - Get current session
- `_auth_destroy_session` - Logout/destroy session
- `_auth_hash_password` - Hash password with Argon2
- `_auth_verify_password` - Verify password hash

**Database:**
- `_db_query` - Execute SQL query
- `_db_execute` - Execute SQL statement
- `_db_transaction_begin` - Start transaction
- `_db_transaction_commit` - Commit transaction
- `_db_transaction_rollback` - Rollback transaction

### 3. Implement each missing function

For each missing function, add it to the linker. Example pattern:

```rust
linker.func_wrap("env", "_req_cookie", |caller: Caller<'_, ServerState>, name_ptr: i32, name_len: i32| -> i32 {
    // 1. Read the cookie name from WASM memory
    let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
    let name = read_string_from_memory(&memory, &caller, name_ptr, name_len);

    // 2. Get the cookie value from the request context
    let state = caller.data();
    let value = state.request_context.cookies.get(&name).cloned().unwrap_or_default();

    // 3. Write the result back to WASM memory and return pointer
    write_string_to_memory(&memory, &mut caller, &value)
})?;
```

### 4. Handle request context

The server needs to pass request context to the WASM execution. Check how the current implementation handles this:

- How is the HTTP request passed to the WASM handler?
- Is there a `ServerState` or similar struct that holds request data?
- How do response values get back to the HTTP layer?

### 5. Test with the Frame demo

After adding the functions, test with:

```bash
cd /Users/earcandy/Documents/Dev/Clean\ Language/clean-framework/examples/complete-demo
cleen frame serve
```

Then visit http://localhost:3000 in a browser.

## Reference Files

### Compiler host bridge definitions
Check these files in clean-language-compiler to see what functions are generated:
- `src/codegen/` - WASM code generation
- `platform-architecture/HOST_BRIDGE.md` - Host bridge specification

### Current server implementation
- `src/main.rs` - Server entry point
- `src/wasm/` or `src/runtime/` - WASM execution
- `host-bridge/` - Existing host function implementations

## Success Criteria

1. `cleen frame serve` starts without "unknown import" errors
2. The Frame demo at http://localhost:3000 loads
3. API endpoints respond correctly
4. All existing tests still pass

## Notes

- Use the existing host function patterns in the codebase
- Follow the memory model for string passing (check platform-architecture/MEMORY_MODEL.md)
- Functions should be no-ops or return sensible defaults if the feature isn't fully implemented yet
- Log warnings for unimplemented functionality rather than crashing
