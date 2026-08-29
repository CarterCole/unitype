;; answers
;;
;; A guest that answers, in as little WebAssembly as says anything.
;;
;; The messages are the subprocess protocol's, so this is the whole contract:
;; three exports and one line of JSON. Written by hand rather than compiled so
;; the ABI can be read rather than taken on trust.
(module
  (memory (export "memory") 1)

  ;; The answer, sitting in memory before anything runs.
  (data (i32.const 1024) "{\"ok\":true,\"value\":[1,2,3]}")

  ;; Room for the request. A real guest would keep a bump pointer; this one
  ;; never reads what it is given, so anywhere past the answer will do.
  (func (export "unitype_alloc") (param $len i32) (result i32)
    i32.const 4096)

  (func (export "unitype_invoke") (param $ptr i32) (param $len i32) (result i64)
    ;; ptr << 32 | len
    (i64.or
      (i64.shl (i64.const 1024) (i64.const 32))
      (i64.const 27)))
)
