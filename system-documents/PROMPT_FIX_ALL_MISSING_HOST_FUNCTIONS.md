# Fix Missing Host Functions in Clean Server

## Problem

The Clean Language compiler generates WASM modules that import certain host functions. The clean-server must provide these, otherwise WASM instantiation fails.

## Two Approaches

There are two ways to fix this:

### Option A: Add Host Functions to Server (Quick Fix)
Implement the missing functions in clean-server as host bridge functions.

### Option B: Make Functions Native in Compiler (Better Long-term)
Implement these functions as native WASM in the compiler's `native_stdlib` module.

**Recommendation**: Option B is preferred for pure computation functions. Option A is required for functions needing system access.

---

## Functions That MUST Be Host Bridge

These require system access and cannot be native WASM:

### File I/O (requires filesystem)
```rust
"file_read", "file_write", "file_append", "file_exists", "file_delete"
"file_size", "file_copy", "file_move", "dir_create", "dir_list"
```

### Network/HTTP Client (requires network)
```rust
"http_get", "http_post", "http_put", "http_patch", "http_delete"
"http_head", "http_options", "http_get_with_headers"
"http_request", "http_set_header", "http_get_status", "http_get_body"
```

### HTTP Server Context (requires server state)
```rust
"_http_route", "_http_listen", "_http_route_protected"
"_req_param", "_req_query", "_req_body", "_req_header"
"_req_method", "_req_path", "_req_cookie"
```

### Session/Auth (requires server state)
```rust
"_session_create", "_session_get", "_session_destroy", "_session_set_cookie"
"_auth_get_session", "_auth_require_auth"
```

### Console I/O (requires stdout/stdin)
```rust
"print", "printl", "print_string", "print_integer", "print_float"
"console_log", "console_error", "console_warn"
"input", "console_input", "input_integer", "input_float", "input_yesno"
```

---

## Functions That SHOULD Be Native WASM

These are pure computation and should be implemented in the compiler's `native_stdlib`:

### String Operations (CURRENTLY IMPORTS - SHOULD BE NATIVE)
```rust
// These are registered as imports but could be native
"string_trim"           // Can be pure WASM
"string_trim_start"     // Can be pure WASM
"string_trim_end"       // Can be pure WASM
"string_replace"        // Can be pure WASM (complex but doable)
"string_split"          // Can be pure WASM
"string_to_upper"       // Can be pure WASM
"string_to_lower"       // Can be pure WASM
```

### Already Native in Compiler
The compiler already implements these natively in `native_stdlib`:
```rust
// These are NATIVE - no host function needed
"string_length", "string_concat", "string_substring"
"string_starts_with", "string_ends_with", "string_contains"
"string_index_of", "string_last_index_of"
"int_to_string", "bool_to_string", "string_to_int"
```

### Math Functions (COULD BE NATIVE)
```rust
// Most can use WASM f64 operations, but some need libm
"math_pow"      // WASM: f64.pow doesn't exist, needs host OR Taylor series
"math_sin"      // Needs host OR Taylor series approximation
"math_cos"      // Needs host OR Taylor series approximation
"math_tan"      // Needs host OR computed from sin/cos
"math_ln"       // Needs host OR algorithm
"math_exp"      // Needs host OR algorithm
// etc.
```

### Array/List Operations
```rust
// Some can be native, some need memory management help
"list.allocate"  // Needs malloc (which IS native in compiler)
"list.push"      // Can be native with malloc
"array_get"      // Can be native (memory access)
"array_set"      // Can be native (memory access)
```

### Type Conversions
```rust
// Some already native, some need host
"float_to_string"   // Complex formatting - easier as host
"string_to_float"   // Complex parsing - easier as host
```

---

## Recommended Fix Strategy

### Step 1: Server - Add REQUIRED host functions
These MUST be in the server (cannot be native):
- All `_req_*` and `_session_*` functions (server context)
- All `file_*` functions (filesystem)
- All `http_*` client functions (network)
- Console I/O functions

### Step 2: Compiler - Make pure functions native
Update `clean-language-compiler/src/codegen/native_stdlib/string_ops.rs` to add:
- `gen_trim()` - trim whitespace from both ends
- `gen_trim_start()` - trim from start
- `gen_trim_end()` - trim from end
- `gen_to_upper()` - convert to uppercase
- `gen_to_lower()` - convert to lowercase
- `gen_replace()` - replace substring
- `gen_split()` - split by delimiter

Then update `builtin_generator.rs` to use `register_function()` instead of `register_import_function()` for these.

### Step 3: Server - Add math functions
Math functions are tricky. Options:
1. Implement in server using Rust's std (easy)
2. Implement in compiler using Taylor series (complex)

Server implementation is simpler.

---

## Quick Server Fix for Current Error

The immediate error is `string_trim_start`. For now, add to server:

```rust
linker.func_wrap("env", "string_trim_start", |mut caller: Caller<'_, State>, ptr: i32| -> i32 {
    let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
    let data = memory.data(&caller);

    // Read length-prefixed string
    let len = i32::from_le_bytes(data[ptr as usize..ptr as usize + 4].try_into().unwrap()) as usize;
    let bytes = &data[ptr as usize + 4..ptr as usize + 4 + len];
    let s = std::str::from_utf8(bytes).unwrap_or("");

    // Trim start
    let trimmed = s.trim_start();

    // Allocate and return new string
    allocate_string(&mut caller, trimmed)
})?;
```

Also add `string_trim_end` and `string_trim` similarly.

---

## Summary

| Category | Must be Host | Can be Native |
|----------|-------------|---------------|
| File I/O | ✅ All | ❌ None |
| HTTP Client | ✅ All | ❌ None |
| HTTP Server | ✅ All | ❌ None |
| Session/Auth | ✅ All | ❌ None |
| Console I/O | ✅ All | ❌ None |
| String ops | Some | Most (trim, replace, split, case) |
| Math | Most (need libm) | Basic ops only |
| Arrays | Memory mgmt | Access ops |
| Type convert | Float parsing | Int/bool to string |

The cleanest solution is a combination:
1. Server provides system-access functions
2. Compiler generates native WASM for pure computation
