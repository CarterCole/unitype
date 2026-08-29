;; lies_about_its_answer
;;
;; Points its answer past the end of its own memory. A host that trusted the
;; number would read whatever is next; there is no "next" inside a sandbox, so
;; this must be caught rather than crash.
(module
  (memory (export "memory") 1)   ;; one page: 65536 bytes

  (func (export "unitype_alloc") (param $len i32) (result i32)
    i32.const 0)

  (func (export "unitype_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.const 100000) (i64.const 32))
      (i64.const 16)))
)
