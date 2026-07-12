//! Regression tests for two Layer 2 HTTP host-bridge canary failures.
//!
//! Dev-queue fingerprints closed by this test:
//!   - cad339dfa7d6fd93  CANARY-HTTP-RESP-HEADERS-INIT-001  (server)
//!   - 5e491149f7c40521  CANARY-HTTP-BUILD-QUERY-001        (server)
//!
//! Background
//! ----------
//! Both bugs live in `host-bridge/src/wasm_linker/http_client.rs`.
//!
//! 1) http.getResponseHeaders() called before any HTTP request should return
//!    an empty string. The Layer 2 canary in the compiler
//!    (`tests/cln/canaries/http_client.cln`) asserts `headers-len:0`. Before
//!    the fix, `HttpLastResponse::default()` initialised `headers_json` to
//!    `"{}"`, so the accessor returned a 2-byte string.
//!
//! 2) http.buildQuery(<query-string>) should return the query string
//!    unchanged. The canary asserts `http.buildQuery("a=1&b=two") == "a=1&b=two"`.
//!    Before the fix, the function assumed a JSON object input; any non-object
//!    (including a plain query string) silently returned `""`.
//!
//! Test approach
//! -------------
//! We instantiate a tiny WAT driver against the real server linker (created by
//! `clean_server::bridge::create_linker`) and invoke each function. The
//! response is a length-prefixed UTF-8 string; the test reads it back from
//! linear memory and compares the payload. This mirrors the approach used by
//! `tests/string_split_test.rs`.

use clean_server::bridge::create_linker;
use clean_server::router::Router;
use clean_server::wasm::WasmState;
use std::sync::Arc;
use wasmtime::{Engine, Module, Store};

// Driver module:
//   - imports the two functions under test (env::http_get_response_headers,
//     env::http_build_query)
//   - exports a 4-page linear memory and mutable __heap_ptr global at 1024
//     so the bridge's malloc-via-caller path can bump-allocate result strings
//   - exports a `malloc` the bridge calls into to allocate results
//   - exports `stamp_len` so the test can build length-prefixed input strings
//   - exports `do_get_headers()` and `do_build_query(ptr, len)` forwarders
//
// The bump allocator starts at address 1024 to leave a scratch region
// [0..1024) for the test to place input strings without collision.
const DRIVER_WAT: &str = r#"
(module
  (import "env" "http_get_response_headers"
    (func $get_headers (result i32)))
  (import "env" "http_build_query"
    (func $build_query (param i32 i32) (result i32)))

  (memory (export "memory") 4)
  (global $heap (export "__heap_ptr") (mut i32) (i32.const 1024))

  (func (export "malloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap
      (i32.and
        (i32.add
          (i32.add (global.get $heap) (local.get $size))
          (i32.const 7))
        (i32.const -8)))
    (local.get $ptr))

  (func (export "do_get_headers") (result i32)
    (call $get_headers))

  ;; do_build_query takes a raw (ptr,len) pair — the bridge reads with
  ;; read_raw_string, which does not require a length prefix on the input.
  (func (export "do_build_query") (param $ptr i32) (param $len i32) (result i32)
    (call $build_query (local.get $ptr) (local.get $len)))
)
"#;

fn read_u32_le(memory: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(memory[offset..offset + 4].try_into().unwrap())
}

fn read_lp_string(memory: &[u8], ptr: usize) -> String {
    let len = read_u32_le(memory, ptr) as usize;
    String::from_utf8(memory[ptr + 4..ptr + 4 + len].to_vec())
        .expect("returned LP string is not valid UTF-8")
}

fn build_driver() -> (Engine, wasmtime::Instance, Store<WasmState>) {
    let engine = Engine::default();
    let module =
        Module::new(&engine, DRIVER_WAT).expect("failed to compile HTTP bridge driver WAT");
    let linker = create_linker(&engine).expect("failed to create server linker");
    let mut store = {
        let router = Arc::new(Router::new());
        Store::new(&engine, WasmState::new(router))
    };
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("failed to instantiate HTTP bridge driver against server linker");
    (engine, instance, store)
}

/// Regression guard for CANARY-HTTP-RESP-HEADERS-INIT-001.
///
/// Before any HTTP request has been executed, `http_get_response_headers`
/// must return an empty string (length 0), not `"{}"`. The compiler canary
/// asserts `headers-len:0`; if this test starts returning 2, the canary in
/// the nightly workflow will start failing again.
#[test]
fn get_response_headers_before_any_request_returns_empty_string() {
    let (_engine, instance, mut store) = build_driver();

    let do_get = instance
        .get_typed_func::<(), i32>(&mut store, "do_get_headers")
        .expect("driver missing do_get_headers export");
    let ptr = do_get
        .call(&mut store, ())
        .expect("http_get_response_headers trapped");
    assert!(ptr > 0, "http_get_response_headers returned null pointer");

    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("driver missing memory export");
    let buf = memory.data(&store);
    let got = read_lp_string(buf, ptr as usize);
    assert_eq!(
        got, "",
        "http_get_response_headers must return \"\" (len 0) before any HTTP \
         request runs — canary CANARY-HTTP-RESP-HEADERS-INIT-001. Got: {:?}",
        got
    );
}

/// Regression guard for CANARY-HTTP-BUILD-QUERY-001.
///
/// A pre-formed query string must pass through unchanged. The canary asserts
/// `http.buildQuery("a=1&b=two") == "a=1&b=two"`. The old implementation
/// tried to parse the input as JSON, failed, and returned "".
#[test]
fn build_query_passes_through_preformed_query_string() {
    let (_engine, instance, mut store) = build_driver();

    // Place the raw input at address 16 — the bridge's read_raw_string does
    // not require a length prefix, only (ptr, len).
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let input = "a=1&b=two";
    let input_ptr = 16i32;
    {
        let buf = memory.data_mut(&mut store);
        buf[input_ptr as usize..input_ptr as usize + input.len()].copy_from_slice(input.as_bytes());
    }

    let do_build = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "do_build_query")
        .expect("driver missing do_build_query export");
    let out_ptr = do_build
        .call(&mut store, (input_ptr, input.len() as i32))
        .expect("http_build_query trapped");
    assert!(out_ptr > 0, "http_build_query returned null pointer");

    let buf = memory.data(&store);
    let got = read_lp_string(buf, out_ptr as usize);
    assert_eq!(
        got, input,
        "http_build_query(\"a=1&b=two\") must return the input unchanged — \
         canary CANARY-HTTP-BUILD-QUERY-001. Got: {:?}",
        got
    );
}

/// Positive test: JSON object input still gets form-urlencoded correctly.
/// The fix must not regress the existing object-encoding path.
#[test]
fn build_query_encodes_json_object_input() {
    let (_engine, instance, mut store) = build_driver();

    // Single-key object so key ordering does not matter for the assertion.
    // Include a space to prove urlencoding is applied (space → '+').
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let input = r#"{"q":"hello world"}"#;
    let input_ptr = 16i32;
    {
        let buf = memory.data_mut(&mut store);
        buf[input_ptr as usize..input_ptr as usize + input.len()].copy_from_slice(input.as_bytes());
    }

    let do_build = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "do_build_query")
        .unwrap();
    let out_ptr = do_build
        .call(&mut store, (input_ptr, input.len() as i32))
        .unwrap();
    assert!(out_ptr > 0);

    let buf = memory.data(&store);
    let got = read_lp_string(buf, out_ptr as usize);
    // form_urlencoded uses '+' for space (as opposed to http_encode_url which
    // uses %20 per RFC 3986). This test locks that behaviour in.
    assert_eq!(
        got, "q=hello+world",
        "http_build_query on JSON object input must form-urlencode key=value \
         pairs. Got: {:?}",
        got
    );
}
