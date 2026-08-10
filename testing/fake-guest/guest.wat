;; testing/fake-guest — the acceptance guest.
;;
;; WHY THIS IS HAND-WRITTEN WAT RATHER THAN COMPILED CLEAN
;;
;; Acceptance calls for a guest built from Clean source. The installed compiler
;; (cln 0.33.154) emits core wasm MODULES, not Component Model components, and
;; generates no clean:http/* imports, so it cannot yet produce a guest for the
;; `clean:host/server@0.1` world. This module stands in until it can.
;;
;; It is deliberately written against the published contract: `wit/guest.wit`
;; imports the same interfaces `../../host.wit` declares (symlinked into
;; wit/deps/http/), so `build.sh` fails if the two ever drift. That makes this
;; a contract test, not just a convenient stub.
;;
;; ROUTES
;;   GET  /            handler 0  -> 200 "hello world"
;;   GET  /users/:id   handler 1  -> 200, body is the captured :id
;;   GET  /events      handler 2  -> SSE stream: two "tick" events, then close
;;   GET  /ws          handler 3  -> WebSocket upgrade, one greeting frame
;;   POST /echo        handler 4  -> 200, echoes the request body

(module
  ;; Canonical ABI: imports are "<interface-name>" / "<func-name>", lowered to
  ;; core signatures — strings become (ptr, len) pairs, and any return value
  ;; larger than one scalar is written to a caller-supplied return area.
  (import "clean:http/routing@0.1.0" "register"
    (func $register (param i32 i32 i32 i32)))
  (import "clean:http/request@0.1.0" "get-body"
    (func $get-body (param i32)))
  (import "clean:http/request@0.1.0" "get-param"
    (func $get-param (param i32 i32 i32)))
  (import "clean:http/response@0.1.0" "set-status"
    (func $set-status (param i32)))
  (import "clean:http/response@0.1.0" "add-header"
    (func $add-header (param i32 i32 i32 i32)))
  (import "clean:http/response@0.1.0" "set-body"
    (func $set-body (param i32 i32)))
  (import "clean:http/websocket@0.1.0" "accept"
    (func $ws-accept (param i32)))
  (import "clean:http/websocket@0.1.0" "send-text"
    (func $ws-send-text (param i64 i32 i32 i32)))
  (import "clean:http/sse@0.1.0" "start"
    (func $sse-start (param i32)))
  (import "clean:http/sse@0.1.0" "send"
    (func $sse-send (param i64 i32 i32 i32 i32 i32 i32 i32)))
  (import "clean:http/sse@0.1.0" "close"
    (func $sse-close (param i64 i32)))

  (memory (export "memory") 1)

  ;; Static strings, at fixed offsets referenced directly below.
  (data (i32.const 0)   "/")                          ;; len 1
  (data (i32.const 8)   "hello world")                ;; len 11
  (data (i32.const 24)  "content-type")               ;; len 12
  (data (i32.const 40)  "text/plain; charset=utf-8")  ;; len 25
  (data (i32.const 80)  "/users/:id")                 ;; len 10
  (data (i32.const 96)  "id")                         ;; len 2
  (data (i32.const 104) "/events")                    ;; len 7
  (data (i32.const 116) "/ws")                        ;; len 3
  (data (i32.const 124) "tick")                       ;; len 4
  (data (i32.const 132) "one")                        ;; len 3
  (data (i32.const 136) "two")                        ;; len 3
  (data (i32.const 144) "hello socket")               ;; len 12
  (data (i32.const 160) "/echo")                      ;; len 5
  (data (i32.const 176) "no id")                      ;; len 5

  ;; Return areas for the canonical ABI, clear of the static data above.
  (global $RET  i32 (i32.const 1024))
  (global $RET2 i32 (i32.const 1088))

  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    ;; This fixture never retains host-allocated memory, so a fixed bump past
    ;; the static data and return areas satisfies the ABI contract.
    (i32.const 2048))

  (func (export "init")
    ;; Method discriminants follow host.wit's enum order: 0 = get, 2 = post.
    (call $register (i32.const 0) (i32.const 0)   (i32.const 1)  (i32.const 0))
    (call $register (i32.const 0) (i32.const 80)  (i32.const 10) (i32.const 1))
    (call $register (i32.const 0) (i32.const 104) (i32.const 7)  (i32.const 2))
    (call $register (i32.const 0) (i32.const 116) (i32.const 3)  (i32.const 3))
    (call $register (i32.const 2) (i32.const 160) (i32.const 5)  (i32.const 4)))

  (func $plain-headers
    (call $add-header
      (i32.const 24) (i32.const 12)
      (i32.const 40) (i32.const 25)))

  (func (export "handle") (param $h i32)
    (local $ptr i32)
    (local $len i32)
    (local $socket i64)
    (local $stream i64)

    (call $set-status (i32.const 200))

    ;; GET /
    (if (i32.eq (local.get $h) (i32.const 0))
      (then
        (call $plain-headers)
        (call $set-body (i32.const 8) (i32.const 11))
        (return)))

    ;; GET /users/:id — echo the captured parameter
    (if (i32.eq (local.get $h) (i32.const 1))
      (then
        (call $plain-headers)
        (call $get-param (i32.const 96) (i32.const 2) (global.get $RET))
        ;; option<string>: discriminant at +0; when 1, (ptr, len) at +4, +8.
        (if (i32.eq (i32.load (global.get $RET)) (i32.const 1))
          (then
            (local.set $ptr (i32.load offset=4 (global.get $RET)))
            (local.set $len (i32.load offset=8 (global.get $RET)))
            (call $set-body (local.get $ptr) (local.get $len)))
          (else
            (call $set-body (i32.const 176) (i32.const 5))))
        (return)))

    ;; GET /events — two SSE events, then close
    (if (i32.eq (local.get $h) (i32.const 2))
      (then
        (call $sse-start (global.get $RET))
        ;; result<u64, e>: discriminant at +0, payload at +8.
        (if (i32.eqz (i32.load (global.get $RET)))
          (then
            (local.set $stream (i64.load offset=8 (global.get $RET)))
            (call $sse-send (local.get $stream)
              (i32.const 124) (i32.const 4)   ;; event-type "tick"
              (i32.const 132) (i32.const 3)   ;; data "one"
              (i32.const 0)   (i32.const 0)   ;; id ""
              (global.get $RET2))
            (call $sse-send (local.get $stream)
              (i32.const 124) (i32.const 4)
              (i32.const 136) (i32.const 3)   ;; data "two"
              (i32.const 0)   (i32.const 0)
              (global.get $RET2))
            (call $sse-close (local.get $stream) (global.get $RET2))))
        (return)))

    ;; GET /ws — accept the upgrade and greet
    (if (i32.eq (local.get $h) (i32.const 3))
      (then
        (call $ws-accept (global.get $RET))
        (if (i32.eqz (i32.load (global.get $RET)))
          (then
            (local.set $socket (i64.load offset=8 (global.get $RET)))
            (call $ws-send-text (local.get $socket)
              (i32.const 144) (i32.const 12)
              (global.get $RET2))))
        (return)))

    ;; POST /echo — echo the request body
    (if (i32.eq (local.get $h) (i32.const 4))
      (then
        (call $plain-headers)
        (call $get-body (global.get $RET))
        ;; list<u8>: (ptr, len) at +0, +4.
        (local.set $ptr (i32.load (global.get $RET)))
        (local.set $len (i32.load offset=4 (global.get $RET)))
        (call $set-body (local.get $ptr) (local.get $len))
        (return)))
  )
)
