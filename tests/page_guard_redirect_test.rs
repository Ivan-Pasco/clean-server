//! Regression test for `PAGE_GUARD_REDIRECT_UNAUTH_UTF8_TRAP`
//! (dashboard fingerprint `bb607124b424`).
//!
//! Background
//! ----------
//! When a page companion's `guard()` returns a redirect directly (the typical
//! "redirect to /login if unauthenticated" pattern), the frame.ui plugin's
//! page-render export calls `_http_redirect` to record the pending redirect
//! and then returns the boxed-any value it received from `guard()` unchanged.
//! That return value is a pointer to a Clean Language record, NOT a
//! length-prefixed UTF-8 string.
//!
//! Before this fix, `call_handler_with_auth` unconditionally fed the handler's
//! return value into `read_string_from_memory`, which interpreted the record
//! bytes as a length-prefixed UTF-8 string and trapped with
//! `Invalid UTF-8 in string: invalid utf-8 sequence of 1 bytes from index 1`.
//! The server then returned HTTP 500 instead of the intended HTTP 302.
//!
//! The fix: if the handler has already signaled a redirect via the host
//! bridge (`pending_redirect.is_some()`), skip reading the body — the
//! redirect path in `handle_request` discards it anyway.

use clean_server::router::Router;
use clean_server::wasm::{RequestContext, WasmInstance};
use std::sync::Arc;

/// Minimal WASM driver that imitates a frame.ui page-render export which
/// forwards a guard-returned redirect:
///
/// 1. Stamps the URL "/login" into memory as raw UTF-8 bytes.
/// 2. Calls `_http_redirect(302, url_ptr, url_len)` to set the pending redirect.
/// 3. Returns a pointer to a "boxed-any" record (a length-prefixed buffer
///    whose body bytes are deliberately invalid UTF-8) — exactly the kind of
///    return value that used to trap the server.
const DRIVER_WAT: &str = r#"
(module
  (import "env" "_http_redirect"
    (func $redirect (param i32 i32 i32) (result i32)))

  (memory (export "memory") 2)
  (global (export "__heap_ptr") (mut i32) (i32.const 65536))

  (func (export "guard_redirect_handler") (result i32)
    ;; Write "/login" at offset 16 (raw bytes — _http_redirect takes ptr+len).
    (i32.store8 (i32.const 16) (i32.const 47))   ;; '/'
    (i32.store8 (i32.const 17) (i32.const 108))  ;; 'l'
    (i32.store8 (i32.const 18) (i32.const 111))  ;; 'o'
    (i32.store8 (i32.const 19) (i32.const 103))  ;; 'g'
    (i32.store8 (i32.const 20) (i32.const 105))  ;; 'i'
    (i32.store8 (i32.const 21) (i32.const 110))  ;; 'n'

    ;; Set the pending redirect on the host side.
    (drop (call $redirect (i32.const 302) (i32.const 16) (i32.const 6)))

    ;; Stamp a fake "boxed-any" at offset 64. The first 4 bytes are a
    ;; little-endian length of 5 (so read_string_from_memory will attempt to
    ;; decode 5 bytes after the prefix). The body bytes are
    ;; [0x20, 0x00, 0xb6, 0x01, 0x00] — a leading space followed by a 0xb6
    ;; continuation byte with no leading byte, which mirrors the exact byte
    ;; pattern reported by the user dashboard (`single space valid prefix`,
    ;; then `00 b6 01 00`).
    (i32.store (i32.const 64) (i32.const 5))
    (i32.store8 (i32.const 68) (i32.const 32))   ;; ' '
    (i32.store8 (i32.const 69) (i32.const 0))
    (i32.store8 (i32.const 70) (i32.const 182))  ;; 0xb6
    (i32.store8 (i32.const 71) (i32.const 1))
    (i32.store8 (i32.const 72) (i32.const 0))

    (i32.const 64)
  )
)
"#;

fn build_request() -> RequestContext {
    RequestContext {
        method: "GET".to_string(),
        path: "/dashboard".to_string(),
        headers: Vec::new(),
        body: String::new(),
        params: Default::default(),
        query: Default::default(),
    }
}

#[test]
fn handler_redirect_with_non_string_return_does_not_trap() {
    let wasm_bytes = wat::parse_str(DRIVER_WAT).expect("failed to assemble page-guard driver WAT");
    let router = Arc::new(Router::new());
    let instance = WasmInstance::from_bytes(&wasm_bytes, router)
        .expect("failed to load page-guard driver WASM");

    let response = instance
        .call_handler_with_auth("guard_redirect_handler", build_request(), None)
        .expect(
            "handler should not error when guard signals a redirect — the i32 return value \
             must be ignored once `_http_redirect` has set pending_redirect",
        );

    assert_eq!(
        response.redirect,
        Some((302, "/login".to_string())),
        "pending redirect must survive the handler call so the server emits HTTP 302"
    );
    assert!(
        response.body.is_empty(),
        "body must be empty when the handler signalled a redirect — read \
         attempted on the boxed-any pointer would otherwise trap with UTF-8 error \
         (got: {:?})",
        response.body
    );
}
