;; never_stops
;;
;; Loops forever. There is no process to kill, so the budget is the only thing
;; that ends this — which is the reason a budget exists.
(module
  (memory (export "memory") 1)

  (func (export "unitype_alloc") (param $len i32) (result i32)
    i32.const 0)

  (func (export "unitype_invoke") (param $ptr i32) (param $len i32) (result i64)
    (loop $forever (br $forever))
    i64.const 0)
)
