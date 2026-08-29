;; nothing_to_call
;;
;; Exports memory and nothing else. Says which export is missing rather than
;; failing vaguely, because the person who wrote the guest is the one reading.
(module
  (memory (export "memory") 1)
)
