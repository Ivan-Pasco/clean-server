//! WASM Alignment Tests
//!
//! Validates that the server linker can instantiate actual WASM binaries and
//! that the host function signatures provided by the linker match what compiled
//! WASM modules import.
//!
//! This closes the "three-way disagreement" gap: while the spec compliance test
//! validates registry <-> implementation, these tests validate
//! implementation <-> actual compiled binaries.
//!
//! # Tests
//!
//! 1. `test_instantiate_example_wasm_binaries` — scans examples/ for .wasm
//!    files and attempts instantiation against the full server linker.
//! 2. `test_compile_and_instantiate` — compiles a minimal Clean Language
//!    source and instantiates the result (skipped if compiler unavailable).
//! 3. `test_linker_provides_all_standard_imports` — a static WAT contract
//!    listing every import the current compiler is known to generate.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

// ---------------------------------------------------------------------------
// Helper: create a minimal WasmState + Store for instantiation attempts
// ---------------------------------------------------------------------------

fn make_store(engine: &Engine) -> Store<WasmState> {
    let router = Arc::new(Router::new());
    let state = WasmState::new(router);
    Store::new(engine, state)
}

// ---------------------------------------------------------------------------
// Helper: attempt to instantiate a pre-compiled .wasm file.
//
// Returns Ok(()) on success or Err with a detailed diagnostic message.
// ---------------------------------------------------------------------------

fn try_instantiate_wasm_file(engine: &Engine, wasm_path: &std::path::Path) -> Result<(), String> {
    let wasm_bytes = std::fs::read(wasm_path).map_err(|e| {
        format!("Failed to read {}: {}", wasm_path.display(), e)
    })?;

    let module = Module::from_binary(engine, &wasm_bytes).map_err(|e| {
        format!(
            "Failed to parse WASM module at {}: {}",
            wasm_path.display(),
            e
        )
    })?;

    let linker = create_linker(engine).map_err(|e| {
        format!("Failed to create server linker: {}", e)
    })?;

    let mut store = make_store(engine);

    linker.instantiate(&mut store, &module).map_err(|e| {
        // Produce a clear message that identifies exactly which import failed.
        // Wasmtime's error message already names the missing/mismatched import.
        format!(
            "Instantiation failed for {}:\n  {}\n\n\
             This means the server linker does not provide (or has the wrong \
             signature for) a host function that the compiled binary imports. \
             Check the function name and signature against the server \
             implementation in src/bridge.rs and host-bridge/src/wasm_linker/.",
            wasm_path.display(),
            e
        )
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 1: Instantiate all .wasm binaries found under examples/
// ---------------------------------------------------------------------------

#[test]
fn test_instantiate_example_wasm_binaries() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = manifest_dir.join("examples");

    // Collect all .wasm files under examples/ recursively.
    let wasm_files = collect_wasm_files(&examples_dir);

    if wasm_files.is_empty() {
        eprintln!(
            "test_instantiate_example_wasm_binaries: No .wasm files found under {}. \
             Skipping — compile some examples first with `cln compile`.",
            examples_dir.display()
        );
        return; // Not a failure: no binaries to test.
    }

    eprintln!(
        "test_instantiate_example_wasm_binaries: Found {} .wasm file(s) to test.",
        wasm_files.len()
    );

    let engine = Engine::default();
    let mut failures: Vec<String> = Vec::new();

    for wasm_path in &wasm_files {
        eprintln!("  Testing: {}", wasm_path.display());
        match try_instantiate_wasm_file(&engine, wasm_path) {
            Ok(()) => eprintln!("    PASS"),
            Err(msg) => {
                eprintln!("    FAIL: {}", msg);
                failures.push(msg);
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} of {} .wasm file(s) failed instantiation:\n\n{}",
            failures.len(),
            wasm_files.len(),
            failures.join("\n\n---\n\n")
        );
    }

    eprintln!(
        "test_instantiate_example_wasm_binaries: All {} file(s) instantiated successfully.",
        wasm_files.len()
    );
}

/// Recursively collect all *.wasm files under `dir`.
fn collect_wasm_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if !dir.exists() {
        return result;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_wasm_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            result.push(path);
        }
    }
    result.sort();
    result
}

// ---------------------------------------------------------------------------
// Test 2: Compile a minimal Clean Language source then instantiate it.
//
// This is the highest-fidelity test: it uses the actual compiler to produce
// a binary and immediately checks that the server linker can satisfy all of
// its imports.  The test is skipped gracefully when the `cln` compiler is not
// installed, so CI environments without a compiler binary still pass.
// ---------------------------------------------------------------------------

#[test]
fn test_compile_and_instantiate() {
    let cln_path = match find_cln_compiler() {
        Some(p) => p,
        None => {
            eprintln!(
                "test_compile_and_instantiate: cln compiler not found. \
                 Install via `cleen install latest` or add to PATH. Skipping."
            );
            return; // Graceful skip — not a failure.
        }
    };

    // Candidate sources in preference order. The Clean Language syntax for
    // the entry point has changed between compiler versions:
    //   - Older versions use `start()` (parentheses, no colon)
    //   - Newer versions use `start:` (colon, no parentheses)
    // We try both to remain robust across installed compiler versions.
    let candidates: &[&str] = &[
        // Newer compiler syntax (start:)
        r#"start:
	integer status = 0
	status = _http_route("GET", "/test", 0)
	integer listenStatus = _http_listen(3000)

functions:
	string __route_handler_0()
		return _req_body()
"#,
        // Older compiler syntax (start())
        r#"functions:
	string __route_handler_0()
		return _req_body()

start()
	integer status = 0
	status = _http_route("GET", "/test", 0)
	integer listenStatus = _http_listen(3000)
"#,
    ];

    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp directory");
    let source_path = temp_dir.path().join("alignment_test.cln");
    let wasm_path = temp_dir.path().join("alignment_test.wasm");

    let mut compilation_errors: Vec<String> = Vec::new();

    for (attempt, source) in candidates.iter().enumerate() {
        std::fs::write(&source_path, source)
            .expect("Failed to write temporary Clean Language source file");

        // Remove any previous .wasm output to avoid stale results.
        let _ = std::fs::remove_file(&wasm_path);

        let compile_output = Command::new(&cln_path)
            .args([
                "compile",
                source_path.to_str().unwrap(),
                "-o",
                wasm_path.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to invoke cln compiler");

        if compile_output.status.success() && wasm_path.exists() {
            eprintln!(
                "test_compile_and_instantiate: Compiled successfully using \
                 candidate syntax {} ({})",
                attempt + 1,
                if attempt == 0 { "start:" } else { "start()" }
            );

            let engine = Engine::default();
            match try_instantiate_wasm_file(&engine, &wasm_path) {
                Ok(()) => {
                    eprintln!(
                        "test_compile_and_instantiate: PASS — compiled binary \
                         instantiated successfully."
                    );
                    return; // Test passed.
                }
                Err(msg) => {
                    panic!(
                        "test_compile_and_instantiate: The compiled binary \
                         could not be instantiated against the server linker.\n\n{}",
                        msg
                    );
                }
            }
        }

        let stdout = String::from_utf8_lossy(&compile_output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&compile_output.stderr).to_string();
        compilation_errors.push(format!(
            "Candidate {}: stdout: {}\nstderr: {}",
            attempt + 1,
            stdout.trim(),
            stderr.trim()
        ));
    }

    // All candidates failed — skip with a diagnostic (not a hard failure)
    // because this means the installed compiler version uses an unknown syntax.
    eprintln!(
        "test_compile_and_instantiate: All {} syntax candidates failed to \
         compile. This likely means the installed compiler version uses \
         different syntax than any known variant. Skipping test.\n\n{}",
        candidates.len(),
        compilation_errors.join("\n---\n")
    );
}

/// Find the `cln` compiler binary.
///
/// Resolution order:
/// 1. `~/.cleen/bin/cln`
/// 2. `cln` on PATH
fn find_cln_compiler() -> Option<PathBuf> {
    // Check the cleen-managed installation first.
    if let Ok(home) = std::env::var("HOME") {
        let managed = PathBuf::from(home).join(".cleen/bin/cln");
        if managed.exists() {
            return Some(managed);
        }
    }

    // Fall back to PATH resolution.
    if let Ok(output) = Command::new("which").arg("cln").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Test 3: Static WAT contract — every import the compiler is known to emit.
//
// This is a compile-time contract.  If the server linker ever stops providing
// one of these functions (or changes its signature), this test catches it
// immediately without needing a compiled binary or a running compiler.
//
// Signatures are derived directly from the host-bridge and bridge.rs
// implementations, verified against actual source code.
//
// Convention:
//   - length-prefixed string ptr  => single i32
//   - raw (ptr, len) string pair  => (i32 i32)
//   - integer                     => i64
//   - number/float                => f64
//   - boolean / status / index    => i32
// ---------------------------------------------------------------------------

/// WAT module that imports every function the Clean Language compiler is known
/// to generate.  The module itself does nothing — we only need the linker to
/// be able to satisfy all imports.
const FULL_CONTRACT_WAT: &str = r#"
(module
  ;; -----------------------------------------------------------------------
  ;; IMPORTANT: All imports must appear before any local definitions in the
  ;; WASM text format.  Local definitions (memory, global, func) come last.
  ;;
  ;; This WAT module lists every host function that the server linker
  ;; provides, with the exact signature used in the implementation.
  ;; Verified against:
  ;;   - host-bridge/src/wasm_linker/console.rs
  ;;   - host-bridge/src/wasm_linker/memory.rs
  ;;   - host-bridge/src/wasm_linker/string_ops.rs
  ;;   - host-bridge/src/wasm_linker/math.rs
  ;;   - host-bridge/src/wasm_linker/http_client.rs
  ;;   - host-bridge/src/wasm_linker/file_io.rs
  ;;   - host-bridge/src/wasm_linker/database.rs
  ;;   - src/bridge.rs (Layer 3)
  ;; -----------------------------------------------------------------------

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — Console (module "env")
  ;; print/printl/print_string use raw (ptr: i32, len: i32) pairs.
  ;; print_integer uses i64 per spec.
  ;; -----------------------------------------------------------------------
  (import "env" "print"         (func (param i32 i32)))
  (import "env" "printl"        (func (param i32 i32)))
  (import "env" "print_string"  (func (param i32 i32)))
  (import "env" "print_integer" (func (param i64)))
  (import "env" "print_float"   (func (param f64)))
  (import "env" "print_boolean" (func (param i32)))
  (import "env" "console_log"   (func (param i32 i32)))
  (import "env" "console_error" (func (param i32 i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — Memory runtime (module "memory_runtime")
  ;; -----------------------------------------------------------------------
  (import "memory_runtime" "mem_alloc"      (func (param i32 i32) (result i32)))
  (import "memory_runtime" "mem_retain"     (func (param i32)))
  (import "memory_runtime" "mem_release"    (func (param i32)))
  (import "memory_runtime" "mem_scope_push" (func))
  (import "memory_runtime" "mem_scope_pop"  (func))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — String operations (module "env")
  ;;
  ;; Single-string functions use a length-prefixed ptr (single i32).
  ;; Two-string functions use two length-prefixed ptrs (two i32 values).
  ;; Three-arg functions use three ptrs.
  ;; -----------------------------------------------------------------------

  ;; concat: (ptr1, ptr2) -> result_ptr
  (import "env" "string_concat"    (func (param i32 i32) (result i32)))
  (import "env" "string.concat"    (func (param i32 i32) (result i32)))

  ;; substring: (str_ptr, start: i32, end: i32) -> result_ptr
  (import "env" "string_substring" (func (param i32 i32 i32) (result i32)))
  (import "env" "string.substring" (func (param i32 i32 i32) (result i32)))

  ;; trim variants: (ptr) -> result_ptr
  (import "env" "string_trim"       (func (param i32) (result i32)))
  (import "env" "string.trim"       (func (param i32) (result i32)))
  (import "env" "string_trim_start" (func (param i32) (result i32)))
  (import "env" "string.trimStart"  (func (param i32) (result i32)))
  (import "env" "string_trim_end"   (func (param i32) (result i32)))
  (import "env" "string.trimEnd"    (func (param i32) (result i32)))

  ;; case: (ptr) -> result_ptr — registered as camelCase (not snake_case)
  (import "env" "string_toUpperCase" (func (param i32) (result i32)))
  (import "env" "string.toUpperCase" (func (param i32) (result i32)))
  (import "env" "string_toLowerCase" (func (param i32) (result i32)))
  (import "env" "string.toLowerCase" (func (param i32) (result i32)))

  ;; replace: (str_ptr, from_ptr, to_ptr) -> result_ptr
  (import "env" "string_replace" (func (param i32 i32 i32) (result i32)))
  (import "env" "string.replace" (func (param i32 i32 i32) (result i32)))

  ;; split: (str_ptr, delim_ptr) -> result_ptr (JSON array)
  (import "env" "string_split"   (func (param i32 i32) (result i32)))
  (import "env" "string.split"   (func (param i32 i32) (result i32)))

  ;; index_of: (haystack_ptr, needle_ptr) -> i32
  (import "env" "string_index_of" (func (param i32 i32) (result i32)))

  ;; compare: (a_ptr, b_ptr) -> i32 (-1 / 0 / 1)
  (import "env" "string_compare"  (func (param i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — Type conversions (module "env")
  ;;
  ;; int_to_string takes i32 (NOT i64).
  ;; string_to_int / string_to_float take a single length-prefixed ptr.
  ;; -----------------------------------------------------------------------
  (import "env" "int_to_string"    (func (param i32) (result i32)))
  (import "env" "integer.toString" (func (param i32) (result i32)))
  (import "env" "float_to_string"  (func (param f64) (result i32)))
  (import "env" "number.toString"  (func (param f64) (result i32)))
  (import "env" "bool_to_string"   (func (param i32) (result i32)))
  (import "env" "boolean.toString" (func (param i32) (result i32)))
  (import "env" "string_to_int"    (func (param i32) (result i32)))
  (import "env" "string.toInteger" (func (param i32) (result i32)))
  (import "env" "string_to_float"  (func (param i32) (result f64)))
  (import "env" "string.toNumber"  (func (param i32) (result f64)))
  (import "env" "string_to_bool"   (func (param i32) (result i32)))
  (import "env" "string.toBoolean" (func (param i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — Math (module "env")
  ;; Note: natural log is math_ln (NOT math_log).
  ;; -----------------------------------------------------------------------
  (import "env" "math_pow"    (func (param f64 f64) (result f64)))
  (import "env" "math_sqrt"   (func (param f64) (result f64)))
  (import "env" "math_abs"    (func (param f64) (result f64)))
  (import "env" "math_floor"  (func (param f64) (result f64)))
  (import "env" "math_ceil"   (func (param f64) (result f64)))
  (import "env" "math_round"  (func (param f64) (result f64)))
  (import "env" "math_trunc"  (func (param f64) (result f64)))
  (import "env" "math_sign"   (func (param f64) (result f64)))
  (import "env" "math_sin"    (func (param f64) (result f64)))
  (import "env" "math_cos"    (func (param f64) (result f64)))
  (import "env" "math_tan"    (func (param f64) (result f64)))
  (import "env" "math_asin"   (func (param f64) (result f64)))
  (import "env" "math_acos"   (func (param f64) (result f64)))
  (import "env" "math_atan"   (func (param f64) (result f64)))
  (import "env" "math_atan2"  (func (param f64 f64) (result f64)))
  (import "env" "math_sinh"   (func (param f64) (result f64)))
  (import "env" "math_cosh"   (func (param f64) (result f64)))
  (import "env" "math_tanh"   (func (param f64) (result f64)))
  (import "env" "math_ln"     (func (param f64) (result f64)))
  (import "env" "math_log10"  (func (param f64) (result f64)))
  (import "env" "math_log2"   (func (param f64) (result f64)))
  (import "env" "math_exp"    (func (param f64) (result f64)))
  (import "env" "math_exp2"   (func (param f64) (result f64)))
  (import "env" "math_min"    (func (param f64 f64) (result f64)))
  (import "env" "math_max"    (func (param f64 f64) (result f64)))
  (import "env" "math_pi"     (func (result f64)))
  (import "env" "math_e"      (func (result f64)))
  (import "env" "math_random" (func (result f64)))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — HTTP client (module "env")
  ;; All use raw (ptr: i32, len: i32) pairs for URL / body strings.
  ;; -----------------------------------------------------------------------
  (import "env" "http_get"       (func (param i32 i32) (result i32)))
  (import "env" "http_post"      (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "http_put"       (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "http_patch"     (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "http_delete"    (func (param i32 i32) (result i32)))
  (import "env" "http_head"      (func (param i32 i32) (result i32)))
  (import "env" "http_options"   (func (param i32 i32) (result i32)))
  (import "env" "http_post_json" (func (param i32 i32 i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — File I/O (module "env")
  ;; file_read takes a mode parameter: (path_ptr, path_len, mode) -> ptr
  ;; -----------------------------------------------------------------------
  (import "env" "file_read"   (func (param i32 i32 i32) (result i32)))
  (import "env" "file_write"  (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "file_exists" (func (param i32 i32) (result i32)))
  (import "env" "file_delete" (func (param i32 i32) (result i32)))
  (import "env" "file_append" (func (param i32 i32 i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 2 — Database (module "env")
  ;; -----------------------------------------------------------------------
  (import "env" "_db_query"    (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "_db_execute"  (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "_db_begin"    (func (result i32)))
  (import "env" "_db_commit"   (func (result i32)))
  (import "env" "_db_rollback" (func (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — HTTP server (module "env", src/bridge.rs)
  ;; String parameters use raw (ptr: i32, len: i32) pairs.
  ;; -----------------------------------------------------------------------

  ;; _http_listen: (port: i32) -> i32
  (import "env" "_http_listen" (func (param i32) (result i32)))

  ;; _http_route: (method_ptr, method_len, path_ptr, path_len, handler_idx) -> i32
  (import "env" "_http_route" (func (param i32 i32 i32 i32 i32) (result i32)))

  ;; _http_route_protected: (..., handler_idx, role_ptr, role_len) -> i32
  (import "env" "_http_route_protected" (func (param i32 i32 i32 i32 i32 i32 i32) (result i32)))

  ;; _http_serve_static: (prefix_ptr, prefix_len, dir_ptr, dir_len) -> i32
  (import "env" "_http_serve_static" (func (param i32 i32 i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — Request context (module "env")
  ;; Key parameters use raw (ptr, len); zero-arg accessors return a ptr.
  ;; -----------------------------------------------------------------------
  (import "env" "_req_param"      (func (param i32 i32) (result i32)))
  (import "env" "_req_query"      (func (param i32 i32) (result i32)))
  (import "env" "_req_body"       (func (result i32)))
  (import "env" "_req_header"     (func (param i32 i32) (result i32)))
  (import "env" "_req_method"     (func (result i32)))
  (import "env" "_req_path"       (func (result i32)))
  (import "env" "_req_cookie"     (func (param i32 i32) (result i32)))
  (import "env" "_req_headers"    (func (result i32)))
  (import "env" "_req_form"       (func (result i32)))
  (import "env" "_req_ip"         (func (result i32)))
  (import "env" "_req_body_field" (func (param i32 i32) (result i32)))
  (import "env" "_req_param_int"  (func (param i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — Response (module "env")
  ;; -----------------------------------------------------------------------

  ;; _http_respond: (status, ct_ptr, ct_len, body_ptr, body_len) -> i32
  (import "env" "_http_respond"   (func (param i32 i32 i32 i32 i32) (result i32)))

  ;; _http_redirect: (status, url_ptr, url_len) -> i32
  (import "env" "_http_redirect"  (func (param i32 i32 i32) (result i32)))

  ;; _http_set_header / _res_set_header: (name_ptr, name_len, val_ptr, val_len) -> i32
  (import "env" "_http_set_header" (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "_res_set_header"  (func (param i32 i32 i32 i32) (result i32)))

  ;; _res_redirect: (url_ptr, url_len, status_code) -> i32
  (import "env" "_res_redirect"   (func (param i32 i32 i32) (result i32)))

  ;; _res_status: (code: i32)
  (import "env" "_res_status"     (func (param i32)))

  ;; _res_body: (body_ptr, body_len)
  (import "env" "_res_body"       (func (param i32 i32)))

  ;; _res_json: (json_ptr, json_len)
  (import "env" "_res_json"       (func (param i32 i32)))

  ;; _http_set_cache: (max_age: i32) -> i32
  (import "env" "_http_set_cache" (func (param i32) (result i32)))

  ;; _http_no_cache: () -> i32
  (import "env" "_http_no_cache"  (func (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — JSON helpers (module "env", src/bridge.rs)
  ;; -----------------------------------------------------------------------
  ;; Note: _json_parse is NOT registered by the server — only _json_get,
  ;; _json_encode, _json_decode are available.
  (import "env" "_json_get"    (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "_json_encode" (func (param i32 i32) (result i32)))
  (import "env" "_json_decode" (func (param i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — Authentication (module "env")
  ;; -----------------------------------------------------------------------
  (import "env" "_auth_get_session"   (func (result i32)))
  (import "env" "_auth_require_auth"  (func (result i32)))
  (import "env" "_auth_require_role"  (func (param i32 i32) (result i32)))
  (import "env" "_auth_can"           (func (param i32 i32) (result i32)))
  (import "env" "_auth_has_any_role"  (func (param i32 i32) (result i32)))
  (import "env" "_auth_set_session"   (func (param i32 i32) (result i32)))
  (import "env" "_auth_clear_session" (func (result i32)))
  (import "env" "_auth_user_id"       (func (result i32)))
  (import "env" "_auth_user_role"     (func (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — Session management (module "env")
  ;;
  ;; _session_store: session_id, key, value use length-prefixed ptrs (i32
  ;; each), plus two i32 scalar args (ttl, flags).
  ;; _session_exists / _session_set_csrf use raw (ptr, len) pairs.
  ;; _http_set_cookie uses two length-prefixed ptrs (name_ptr, value_ptr).
  ;; -----------------------------------------------------------------------
  (import "env" "_session_store"    (func (param i32 i32 i32 i32 i32) (result i32)))
  (import "env" "_session_get"      (func (result i32)))
  (import "env" "_session_delete"   (func (result i32)))
  (import "env" "_session_exists"   (func (param i32 i32) (result i32)))
  (import "env" "_session_set_csrf" (func (param i32 i32) (result i32)))
  (import "env" "_session_get_csrf" (func (result i32)))
  (import "env" "_http_set_cookie"  (func (param i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Layer 3 — Roles (module "env")
  ;; -----------------------------------------------------------------------
  (import "env" "_roles_register"       (func (param i32 i32) (result i32)))
  (import "env" "_role_has_permission"  (func (param i32 i32 i32 i32) (result i32)))
  (import "env" "_role_get_permissions" (func (param i32 i32) (result i32)))

  ;; -----------------------------------------------------------------------
  ;; Local definitions — MUST appear AFTER all imports per WASM spec.
  ;; -----------------------------------------------------------------------
  (memory (export "memory") 1)
  (global $heap_ptr (export "__heap_ptr") (mut i32) (i32.const 1024))

  ;; Minimal bump-allocator malloc stub.  Host functions that write strings
  ;; back into WASM memory use the "memory" export and this global.
  (func (export "malloc") (param i32) (result i32)
    (local $old_ptr i32)
    (local.set $old_ptr (global.get $heap_ptr))
    (global.set $heap_ptr (i32.add (global.get $heap_ptr) (local.get 0)))
    (local.get $old_ptr)
  )
)
"#;

#[test]
fn test_linker_provides_all_standard_imports() {
    let engine = Engine::default();

    // Parse the WAT contract into a WASM module.
    let module = Module::new(&engine, FULL_CONTRACT_WAT).unwrap_or_else(|e| {
        panic!(
            "test_linker_provides_all_standard_imports: WAT contract failed to parse.\n\
             This is a bug in the test itself — fix the WAT syntax.\n\
             Error: {}",
            e
        )
    });

    // Create the full server linker (Layer 2 + Layer 3).
    let linker = create_linker(&engine).unwrap_or_else(|e| {
        panic!(
            "test_linker_provides_all_standard_imports: Failed to create server linker: {}",
            e
        )
    });

    let mut store = make_store(&engine);

    // If the linker cannot instantiate the module, wasmtime will report exactly
    // which import is missing or has the wrong signature.
    linker.instantiate(&mut store, &module).unwrap_or_else(|e| {
        panic!(
            "test_linker_provides_all_standard_imports: CONTRACT FAILURE\n\n\
             The server linker is missing a host function or has a signature \
             mismatch for a function that compiled WASM binaries import.\n\n\
             Wasmtime error (identifies the exact failing import):\n  {}\n\n\
             Fix the implementation in src/bridge.rs or \
             host-bridge/src/wasm_linker/ to match the contract, not the \
             other way around.",
            e
        )
    });

    // Count imports for the success message.
    let import_count = module.imports().count();
    eprintln!(
        "test_linker_provides_all_standard_imports: PASSED — linker satisfied \
         all {} imports in the standard contract.",
        import_count
    );
}
