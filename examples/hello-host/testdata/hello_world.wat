(module
  (import "env" "print" (func $print (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello world from guest")
  (func (export "run")
    i32.const 0
    i32.const 22
    call $print))
