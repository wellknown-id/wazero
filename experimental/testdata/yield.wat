(module
  ;; Import a host function that yields execution.
  ;; It takes no params and returns an i32 result.
  (import "example" "async_work" (func $async_work (result i32)))

  ;; A simple function that calls async_work and returns the result + 100.
  (func (export "run") (result i32)
    (i32.add
      (call $async_work)
      (i32.const 100)
    )
  )

  ;; A function that calls async_work twice and returns sum.
  (func (export "run_twice") (result i32)
    (i32.add
      (call $async_work)
      (call $async_work)
    )
  )

  (memory (export "memory") 1 1)
)
