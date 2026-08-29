;; reports_failure
;;
;; A guest whose proof fails, and says so properly.
;;
;; Failing is not the same as breaking. This one answers correctly with an
;; answer that reports a failure, which must reach the caller as the proof
;; failing rather than as the environment being unreachable.
(module
  (memory (export "memory") 1)

  ;; The answer, sitting in memory before anything runs.
  (data (i32.const 1024) "{\"ok\":false,\"error\":{\"code\":\"nope\",\"message\":\"this guest proves nothing\"}}")

  ;; Room for the request. A real guest would keep a bump pointer; this one
  ;; never reads what it is given, so anywhere past the answer will do.
  (func (export "unitype_alloc") (param $len i32) (result i32)
    i32.const 4096)

  (func (export "unitype_invoke") (param $ptr i32) (param $len i32) (result i64)
    ;; ptr << 32 | len
    (i64.or
      (i64.shl (i64.const 1024) (i64.const 32))
      (i64.const 74)))
)
