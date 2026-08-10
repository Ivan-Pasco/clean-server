;; testing/fake-bridge — an in-memory component for composition tests.
;;
;; Exports clean:fake-bridge/store, which the acceptance guest imports. That is
;; enough to exercise the whole Phase 3 path: discovery reads it, validation
;; checks it exports what [bridges] promised, and WAC wires its export into the
;; guest's matching import.
(module
  (memory (export "memory") 1)
  (global $counter (mut i32) (i32.const 0))

  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (i32.const 1024))

  (func (export "clean:fake-bridge/store@0.1.0#bump") (result i32)
    (global.set $counter (i32.add (global.get $counter) (i32.const 1)))
    (global.get $counter))
)
