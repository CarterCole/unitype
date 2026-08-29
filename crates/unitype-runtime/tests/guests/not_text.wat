;; not_text
;;
;; Answers with bytes that are not UTF-8. Reading them as text would be a panic
;; in some hosts and a replacement character in others; here it is an error
;; naming the guest.
(module
  (memory (export "memory") 1)

  (data (i32.const 1024) "\ff\fe\ff")

  (func (export "unitype_alloc") (param $len i32) (result i32)
    i32.const 4096)

  (func (export "unitype_invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.const 1024) (i64.const 32))
      (i64.const 3)))
)
