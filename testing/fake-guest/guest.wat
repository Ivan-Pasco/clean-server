;; testing/fake-guest — the M0 acceptance guest.
;;
;; WHY THIS IS HAND-WRITTEN WAT RATHER THAN COMPILED CLEAN
;;
;; M0 acceptance calls for a guest built from Clean source. The installed
;; compiler (cln 0.33.154) emits core wasm MODULES, not Component Model
;; components, and generates no clean:http/* imports, so it cannot yet produce
;; a guest for the `clean:host/server@0.1` world. This module stands in until
;; it can.
;;
;; It is deliberately written against the published contract: `wit/guest.wit`
;; imports the same interfaces `../../host.wit` declares (symlinked into
;; wit/deps/http/), so `build.sh` fails if the two ever drift. That makes this
;; a contract test, not just a convenient stub.
;;
;; Behavior: registers `GET /` as handler 0; on invocation responds 200 with
;; "hello world" and an explicit text/plain content type.

(module
  ;; Canonical ABI: imports are "<interface-name>" / "<func-name>", lowered to
  ;; core signatures (strings become ptr+len pairs).
  (import "clean:http/routing@0.1.0" "register"
    (func $register (param i32 i32 i32 i32)))
  (import "clean:http/response@0.1.0" "set-status"
    (func $set-status (param i32)))
  (import "clean:http/response@0.1.0" "add-header"
    (func $add-header (param i32 i32 i32 i32)))
  (import "clean:http/response@0.1.0" "set-body"
    (func $set-body (param i32 i32)))

  (memory (export "memory") 1)
  (data (i32.const 0) "/")
  (data (i32.const 8) "hello world")
  (data (i32.const 24) "content-type")
  (data (i32.const 40) "text/plain; charset=utf-8")

  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (i32.const 256))

  (func (export "init")
    (call $register (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 0)))

  (func (export "handle") (param $h i32)
    (call $set-status (i32.const 200))
    (call $add-header (i32.const 24) (i32.const 12) (i32.const 40) (i32.const 25))
    (call $set-body (i32.const 8) (i32.const 11)))
)
