package main

/*
#include <stdint.h>
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"unsafe"

	"github.com/tetratelabs/wazero"
)

//export wazero_run
func wazero_run(codePtr *C.uint8_t, codeLen C.int, funcName *C.char) C.int {
	code := C.GoBytes(unsafe.Pointer(codePtr), codeLen)
	fname := C.GoString(funcName)

	ctx := context.Background()
	r := wazero.NewRuntime(ctx)
	defer r.Close(ctx)

	mod, err := r.Instantiate(ctx, code)
	if err != nil {
		return -1
	}

	fn := mod.ExportedFunction(fname)
	if fn == nil {
		return -2
	}

	if _, err := fn.Call(ctx); err != nil {
		return -3
	}
	return 0
}

func main() {}
