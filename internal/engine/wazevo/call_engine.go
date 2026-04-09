package wazevo

import (
	"context"
	"errors"
	"fmt"
	"reflect"
	"runtime"
	"sync/atomic"
	"unsafe"

	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/engine/wazevo/wazevoapi"
	"github.com/tetratelabs/wazero/internal/engine/yieldstate"
	"github.com/tetratelabs/wazero/internal/expctxkeys"
	"github.com/tetratelabs/wazero/internal/internalapi"
	"github.com/tetratelabs/wazero/internal/wasm"
	"github.com/tetratelabs/wazero/internal/wasmdebug"
	"github.com/tetratelabs/wazero/internal/wasmruntime"
)

type (
	// callEngine implements api.Function.
	callEngine struct {
		internalapi.WazeroOnly
		stack []byte
		// stackTop is the pointer to the *aligned* top of the stack. This must be updated
		// whenever the stack is changed. This is passed to the assembly function
		// at the very beginning of api.Function Call/CallWithStack.
		stackTop uintptr
		// executable is the pointer to the executable code for this function.
		executable         *byte
		preambleExecutable *byte
		// parent is the *moduleEngine from which this callEngine is created.
		parent *moduleEngine
		// indexInModule is the index of the function in the module.
		indexInModule wasm.Index
		// sizeOfParamResultSlice is the size of the parameter/result slice.
		sizeOfParamResultSlice int
		requiredParams         int
		// execCtx holds various information to be read/written by assembly functions.
		execCtx executionContext
		// execCtxPtr holds the pointer to the executionContext which doesn't change after callEngine is created.
		execCtxPtr        uintptr
		numberOfResults   int
		stackIteratorImpl stackIterator
		// yieldState tracks whether this call engine is idle, suspended (yielded),
		// or currently being resumed. Used to prevent concurrent misuse.
		yieldState atomic.Int32
	}

	// executionContext is the struct to be read/written by assembly functions.
	executionContext struct {
		// exitCode holds the wazevoapi.ExitCode describing the state of the function execution.
		exitCode wazevoapi.ExitCode
		// callerModuleContextPtr holds the moduleContextOpaque for Go function calls.
		callerModuleContextPtr *byte
		// originalFramePointer holds the original frame pointer of the caller of the assembly function.
		originalFramePointer uintptr
		// originalStackPointer holds the original stack pointer of the caller of the assembly function.
		originalStackPointer uintptr
		// goReturnAddress holds the return address to go back to the caller of the assembly function.
		goReturnAddress uintptr
		// stackBottomPtr holds the pointer to the bottom of the stack.
		stackBottomPtr *byte
		// goCallReturnAddress holds the return address to go back to the caller of the Go function.
		goCallReturnAddress *byte
		// stackPointerBeforeGoCall holds the stack pointer before calling a Go function.
		stackPointerBeforeGoCall *uint64
		// stackGrowRequiredSize holds the required size of stack grow.
		stackGrowRequiredSize uintptr
		// memoryGrowTrampolineAddress holds the address of memory grow trampoline function.
		memoryGrowTrampolineAddress *byte
		// stackGrowCallTrampolineAddress holds the address of stack grow trampoline function.
		stackGrowCallTrampolineAddress *byte
		// checkModuleExitCodeTrampolineAddress holds the address of check-module-exit-code function.
		checkModuleExitCodeTrampolineAddress *byte
		// savedRegisters is the opaque spaces for save/restore registers.
		// We want to align 16 bytes for each register, so we use [64][2]uint64.
		savedRegisters [64][2]uint64
		// goFunctionCallCalleeModuleContextOpaque is the pointer to the target Go function's moduleContextOpaque.
		goFunctionCallCalleeModuleContextOpaque uintptr
		// tableGrowTrampolineAddress holds the address of table grow trampoline function.
		tableGrowTrampolineAddress *byte
		// refFuncTrampolineAddress holds the address of ref-func trampoline function.
		refFuncTrampolineAddress *byte
		// memmoveAddress holds the address of memmove function implemented by Go runtime. See memmove.go.
		memmoveAddress uintptr
		// framePointerBeforeGoCall holds the frame pointer before calling a Go function. Note: only used in amd64.
		framePointerBeforeGoCall uintptr
		// memoryWait32TrampolineAddress holds the address of memory_wait32 trampoline function.
		memoryWait32TrampolineAddress *byte
		// memoryWait32TrampolineAddress holds the address of memory_wait64 trampoline function.
		memoryWait64TrampolineAddress *byte
		// memoryNotifyTrampolineAddress holds the address of the memory_notify trampoline function.
		memoryNotifyTrampolineAddress *byte
		// fuel holds the remaining fuel for deterministic CPU metering.
		// Compiled code decrements this at function entries and loop back-edges.
		// When fuel drops below zero, the compiled code exits with ExitCodeFuelExhausted.
		// A value of 0 means fuel metering is disabled for this execution.
		//
		// We use int64 (not uint64) so that the exhaustion check in generated native
		// code is a simple signed comparison: sub + b.lt is a single conditional
		// branch on both amd64 and arm64, whereas unsigned underflow detection would
		// require an extra carry/overflow check.
		fuel int64
	}
)

func (c *callEngine) requiredInitialStackSize() int {
	const initialStackSizeDefault = 10240
	stackSize := initialStackSizeDefault
	paramResultInBytes := c.sizeOfParamResultSlice * 8 * 2 // * 8 because uint64 is 8 bytes, and *2 because we need both separated param/result slots.
	required := paramResultInBytes + 32 + 16               // 32 is enough to accommodate the call frame info, and 16 exists just in case when []byte is not aligned to 16 bytes.
	if required > stackSize {
		stackSize = required
	}
	return stackSize
}

func (c *callEngine) init() {
	stackSize := c.requiredInitialStackSize()
	if wazevoapi.StackGuardCheckEnabled {
		stackSize += wazevoapi.StackGuardCheckGuardPageSize
	}
	c.stack = make([]byte, stackSize)
	c.stackTop = alignedStackTop(c.stack)
	if wazevoapi.StackGuardCheckEnabled {
		c.execCtx.stackBottomPtr = &c.stack[wazevoapi.StackGuardCheckGuardPageSize]
	} else {
		c.execCtx.stackBottomPtr = &c.stack[0]
	}
	c.execCtxPtr = uintptr(unsafe.Pointer(&c.execCtx))
}

// alignedStackTop returns 16-bytes aligned stack top of given stack.
// 16 bytes should be good for all platform (arm64/amd64).
func alignedStackTop(s []byte) uintptr {
	stackAddr := uintptr(unsafe.Pointer(&s[len(s)-1]))
	return stackAddr - (stackAddr & (16 - 1))
}

// Definition implements api.Function.
func (c *callEngine) Definition() api.FunctionDefinition {
	return c.parent.module.Source.FunctionDefinition(c.indexInModule)
}

// Call implements api.Function.
func (c *callEngine) Call(ctx context.Context, params ...uint64) ([]uint64, error) {
	if c.requiredParams != len(params) {
		return nil, fmt.Errorf("expected %d params, but passed %d", c.requiredParams, len(params))
	}
	paramResultSlice := make([]uint64, c.sizeOfParamResultSlice)
	copy(paramResultSlice, params)
	if err := c.callWithStack(ctx, paramResultSlice); err != nil {
		return nil, err
	}
	return paramResultSlice[:c.numberOfResults], nil
}

func (c *callEngine) addFrame(builder wasmdebug.ErrorBuilder, addr uintptr) (def api.FunctionDefinition, listener experimental.FunctionListener) {
	eng := c.parent.parent.parent
	cm := eng.compiledModuleOfAddr(addr)
	if cm == nil {
		// This case, the module might have been closed and deleted from the engine.
		// We fall back to searching the imported modules that can be referenced from this callEngine.

		// First, we check itself.
		if checkAddrInBytes(addr, c.parent.parent.executable) {
			cm = c.parent.parent
		} else {
			// Otherwise, search all imported modules. TODO: maybe recursive, but not sure it's useful in practice.
			p := c.parent
			for i := range p.importedFunctions {
				candidate := p.importedFunctions[i].me.parent
				if checkAddrInBytes(addr, candidate.executable) {
					cm = candidate
					break
				}
			}
		}
	}

	if cm != nil {
		index := cm.functionIndexOf(addr)
		def = cm.module.FunctionDefinition(cm.module.ImportFunctionCount + index)
		var sources []string
		if dw := cm.module.DWARFLines; dw != nil {
			sourceOffset := cm.getSourceOffset(addr)
			sources = dw.Line(sourceOffset)
		}
		builder.AddFrame(def.DebugName(), def.ParamTypes(), def.ResultTypes(), sources)
		if len(cm.listeners) > 0 {
			listener = cm.listeners[index]
		}
	}
	return
}

// CallWithStack implements api.Function.
func (c *callEngine) CallWithStack(ctx context.Context, paramResultStack []uint64) (err error) {
	if c.sizeOfParamResultSlice > len(paramResultStack) {
		return fmt.Errorf("need %d params, but stack size is %d", c.sizeOfParamResultSlice, len(paramResultStack))
	}
	return c.callWithStack(ctx, paramResultStack)
}

// CallWithStack implements api.Function.
func (c *callEngine) callWithStack(ctx context.Context, paramResultStack []uint64) (err error) {
	p := c.parent
	m := p.module
	if c.yieldState.Load() != compilerYieldStateIdle {
		return m.FailIfSuspended()
	}
	if err := m.FailIfSuspended(); err != nil {
		return err
	}
	if experimental.GetTimeProvider(ctx) == nil && m.TimeProvider != nil {
		ctx = experimental.WithTimeProvider(ctx, m.TimeProvider)
	}
	snapshotEnabled := ctx.Value(expctxkeys.EnableSnapshotterKey{}) != nil
	if snapshotEnabled {
		ctx = context.WithValue(ctx, expctxkeys.SnapshotterKey{}, c)
	}

	// Enable async yield/resume if requested.
	yieldEnabled := ctx.Value(expctxkeys.EnableYielderKey{}) != nil

	if wazevoapi.StackGuardCheckEnabled {
		defer func() {
			wazevoapi.CheckStackGuardPage(c.stack)
		}()
	}

	ensureTermination := p.parent.ensureTermination
	if ensureTermination {
		select {
		case <-ctx.Done():
			// If the provided context is already done, close the module and return the error.
			m.CloseWithCtxErr(ctx)
			return m.FailIfClosed()
		default:
		}
	}

	var paramResultPtr *uint64
	if len(paramResultStack) > 0 {
		paramResultPtr = &paramResultStack[0]
	}

	// Initialize fuel for deterministic CPU metering.
	// Priority: FuelController in context > compiled module fuel config.
	// Declared before defer so the closure captures these for consumption reporting on trap.
	var fuelCtrl experimental.FuelController
	var initialFuel int64
	var runtimeFuel int64
	var fuelAdded int64
	fuelObserver := experimental.GetFuelObserver(ctx)
	fuelObserverCtx := ctx
	if fc, ok := ctx.Value(expctxkeys.FuelControllerKey{}).(experimental.FuelController); ok && fc != nil {
		fuelCtrl = fc
		initialFuel = fc.Budget()
		if initialFuel > 0 {
			runtimeFuel = initialFuel
		} else {
			runtimeFuel = 1<<63 - 1
		}
	} else if p.parent.fuelEnabled {
		initialFuel = p.parent.fuel
		runtimeFuel = initialFuel
	}
	if runtimeFuel > 0 {
		c.execCtx.fuel = runtimeFuel
		// Inject a FuelAccessor into the context so that host functions called
		// during this execution can inspect and modify the fuel counter via
		// experimental.AddFuel / experimental.RemainingFuel.
		if initialFuel > 0 {
			ctx = context.WithValue(ctx, expctxkeys.FuelAccessorKey{}, &expctxkeys.FuelAccessor{Ptr: &c.execCtx.fuel, Module: m, Added: &fuelAdded})
			notifyFuelObserver(ctx, fuelObserverCtx, fuelObserver, experimental.FuelObservation{
				Module:    m,
				Event:     experimental.FuelEventBudgeted,
				Budget:    initialFuel,
				Remaining: initialFuel,
			})
		}
	}

	defer func() {
		r := recover()
		if s, ok := r.(*snapshot); ok {
			// A snapshot that wasn't handled was created by a different call engine possibly from a nested wasm invocation,
			// let it propagate up to be handled by the caller.
			panic(s)
		}

		// Check for yield signal.
		if ys, ok := r.(*compilerYieldSignal); ok && ys.ce == c {
			// Cooperative yield: capture native stack state and return
			// a YieldError to the embedder.
			returnAddress := c.execCtx.goCallReturnAddress
			oldTop, oldSp := c.stackTop, uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall))
			newSP, newFP, newTop, newStack := c.cloneStack(uintptr(len(c.stack)) + 16)
			adjustClonedStack(oldSp, oldTop, newSP, newFP, newTop)

			resumer := &compilerResumer{
				sp:                  newSP,
				fp:                  newFP,
				top:                 newTop,
				savedRegisters:      c.execCtx.savedRegisters,
				returnAddress:       returnAddress,
				stack:               newStack,
				ce:                  c,
				module:              m,
				paramResultStack:    paramResultStack,
				fuelCtrl:            fuelCtrl,
				initialFuel:         initialFuel,
				fuelAdded:           fuelAdded,
				fuelObserver:        fuelObserver,
				fuelObserverCtx:     ctx,
				expectedHostResults: c.currentHostFunctionResultCount(),
				yieldCount:          1,
				yieldObserver:       experimental.GetYieldObserver(ctx),
				yieldObserverCtx:    ctx,
				yieldClock:          experimental.GetTimeProvider(ctx),
				yieldedAtNanos:      yieldNanotime(experimental.GetTimeProvider(ctx)),
			}
			c.yieldState.Store(compilerYieldStateSuspended)
			m.MarkSuspended()
			notifyYieldObserver(ctx, ctx, resumer.yieldObserver, m, experimental.YieldEventYielded, resumer.yieldCount, resumer.expectedHostResults, nil, 0)
			err = experimental.NewYieldError(resumer)
			return
		}

		if r != nil {
			type listenerForAbort struct {
				def api.FunctionDefinition
				lsn experimental.FunctionListener
			}

			var listeners []listenerForAbort
			builder := wasmdebug.NewErrorBuilder()
			def, lsn := c.addFrame(builder, uintptr(unsafe.Pointer(c.execCtx.goCallReturnAddress)))
			if lsn != nil {
				listeners = append(listeners, listenerForAbort{def, lsn})
			}

			if c.execCtx.stackPointerBeforeGoCall != nil {
				returnAddrs := unwindStack(
					uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)),
					c.execCtx.framePointerBeforeGoCall,
					c.stackTop,
					nil,
				)
				for _, retAddr := range returnAddrs[:len(returnAddrs)-1] { // the last return addr is the trampoline, so we skip it.
					def, lsn = c.addFrame(builder, retAddr)
					if lsn != nil {
						listeners = append(listeners, listenerForAbort{def, lsn})
					}
				}
			}
			err = builder.FromRecovered(r)

			for _, lsn := range listeners {
				lsn.lsn.Abort(ctx, m, lsn.def, err)
			}
		} else {
			if err != wasmruntime.ErrRuntimeStackOverflow { // Stackoverflow case shouldn't be panic (to avoid extreme stack unwinding).
				err = c.parent.module.FailIfClosed()
			}
		}

		if err != nil {
			notifyTrapObserver(ctx, m, err)
			if errors.Is(err, wasmruntime.ErrRuntimeFuelExhausted) {
				budget := currentFuelBudget(initialFuel, fuelAdded)
				notifyFuelObserver(ctx, fuelObserverCtx, fuelObserver, experimental.FuelObservation{
					Module:    m,
					Event:     experimental.FuelEventExhausted,
					Budget:    budget,
					Consumed:  budget - c.execCtx.fuel,
					Remaining: c.execCtx.fuel,
				})
			} else if initialFuel > 0 {
				budget := currentFuelBudget(initialFuel, fuelAdded)
				notifyFuelObserver(ctx, fuelObserverCtx, fuelObserver, experimental.FuelObservation{
					Module:    m,
					Event:     experimental.FuelEventConsumed,
					Budget:    budget,
					Consumed:  budget - c.execCtx.fuel,
					Remaining: c.execCtx.fuel,
				})
			}
			// Report fuel consumption to the FuelController even on error/trap.
			if fuelCtrl != nil && initialFuel > 0 {
				consumed := currentFuelBudget(initialFuel, fuelAdded) - c.execCtx.fuel
				if consumed > 0 {
					fuelCtrl.Consumed(consumed)
				}
			}
			// Ensures that we can reuse this callEngine even after an error.
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
		}
	}()

	if ensureTermination {
		done := m.CloseModuleOnCanceledOrTimeout(ctx)
		defer done()
	}

	if c.stackTop&(16-1) != 0 {
		panic("BUG: stack must be aligned to 16 bytes")
	}
	entrypoint(c.preambleExecutable, c.executable, c.execCtxPtr, c.parent.opaquePtr, paramResultPtr, c.stackTop)
	for {
		switch ec := c.execCtx.exitCode; ec & wazevoapi.ExitCodeMask {
		case wazevoapi.ExitCodeOK:
			if initialFuel > 0 {
				budget := currentFuelBudget(initialFuel, fuelAdded)
				notifyFuelObserver(ctx, fuelObserverCtx, fuelObserver, experimental.FuelObservation{
					Module:    m,
					Event:     experimental.FuelEventConsumed,
					Budget:    budget,
					Consumed:  budget - c.execCtx.fuel,
					Remaining: c.execCtx.fuel,
				})
			}
			// Report fuel consumption to the FuelController if active.
			if fuelCtrl != nil && initialFuel > 0 {
				consumed := currentFuelBudget(initialFuel, fuelAdded) - c.execCtx.fuel
				if consumed > 0 {
					fuelCtrl.Consumed(consumed)
				}
			}
			return nil
		case wazevoapi.ExitCodeGrowStack:
			oldsp := uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall))
			oldTop := c.stackTop
			oldStack := c.stack
			var newsp, newfp uintptr
			if wazevoapi.StackGuardCheckEnabled {
				newsp, newfp, err = c.growStackWithGuarded()
			} else {
				newsp, newfp, err = c.growStack()
			}
			if err != nil {
				return err
			}
			adjustClonedStack(oldsp, oldTop, newsp, newfp, c.stackTop)
			// Old stack must be alive until the new stack is adjusted.
			runtime.KeepAlive(oldStack)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr, newsp, newfp)
		case wazevoapi.ExitCodeGrowMemory:
			mod := c.callerModuleInstance()
			mem := mod.MemoryInstance
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			argRes := &s[0]
			if res, ok := mem.Grow(uint32(*argRes)); !ok {
				*argRes = uint64(0xffffffff) // = -1 in signed 32-bit integer.
			} else {
				*argRes = uint64(res)
			}
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr, uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeTableGrow:
			mod := c.callerModuleInstance()
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			tableIndex, num, ref := uint32(s[0]), uint32(s[1]), uintptr(s[2])
			table := mod.Tables[tableIndex]
			s[0] = uint64(uint32(int32(table.Grow(num, ref))))
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoFunction:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			callerModule := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, callerModule)
			if !c.allowHostCall(callCtx, callerModule, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			hostCtx := c.withHostYielder(callCtx, callerModule, index)
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, goCallStackView(c.execCtx.stackPointerBeforeGoCall))
			}()
			// Back to the native code.
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoFunctionWithListener:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			callerModule := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, callerModule)
			if !c.allowHostCall(callCtx, callerModule, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			listeners := hostModuleListenersSliceFromOpaque(c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			// Call Listener.Before.
			listener := listeners[index]
			def := c.hostFunctionDefinition(index)
			listener.Before(callCtx, callerModule, def, s, c.stackIterator(true))
			hostCtx := c.withHostYielder(callCtx, callerModule, index)
			// Call into the Go function.
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, s)
			}()
			// Call Listener.After.
			listener.After(callCtx, callerModule, def, s)
			// Back to the native code.
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoModuleFunction:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			mod := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, mod)
			if !c.allowHostCall(callCtx, mod, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoModuleFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			hostCtx := c.withHostYielder(callCtx, mod, index)
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, mod, goCallStackView(c.execCtx.stackPointerBeforeGoCall))
			}()
			// Back to the native code.
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoModuleFunctionWithListener:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			callerModule := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, callerModule)
			if !c.allowHostCall(callCtx, callerModule, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoModuleFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			listeners := hostModuleListenersSliceFromOpaque(c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			// Call Listener.Before.
			listener := listeners[index]
			def := c.hostFunctionDefinition(index)
			listener.Before(callCtx, callerModule, def, s, c.stackIterator(true))
			hostCtx := c.withHostYielder(callCtx, callerModule, index)
			// Call into the Go function.
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, callerModule, s)
			}()
			// Call Listener.After.
			listener.After(callCtx, callerModule, def, s)
			// Back to the native code.
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallListenerBefore:
			stack := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			index := wasm.Index(stack[0])
			mod := c.callerModuleInstance()
			listener := mod.Engine.(*moduleEngine).listeners[index]
			def := mod.Source.FunctionDefinition(index + mod.Source.ImportFunctionCount)
			listener.Before(ctx, mod, def, stack[1:], c.stackIterator(false))
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallListenerAfter:
			stack := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			index := wasm.Index(stack[0])
			mod := c.callerModuleInstance()
			listener := mod.Engine.(*moduleEngine).listeners[index]
			def := mod.Source.FunctionDefinition(index + mod.Source.ImportFunctionCount)
			listener.After(ctx, mod, def, stack[1:])
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCheckModuleExitCode:
			// Note: this operation must be done in Go, not native code. The reason is that
			// native code cannot be preempted and that means it can block forever if there are not
			// enough OS threads (which we don't have control over).
			if err := m.FailIfClosed(); err != nil {
				panic(err)
			}
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeRefFunc:
			mod := c.callerModuleInstance()
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			funcIndex := wasm.Index(s[0])
			ref := mod.Engine.FunctionInstanceReference(funcIndex)
			s[0] = uint64(ref)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeMemoryWait32:
			mod := c.callerModuleInstance()
			mem := mod.MemoryInstance
			if !mem.Shared {
				panic(wasmruntime.ErrRuntimeExpectedSharedMemory)
			}

			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			timeout, exp, addr := int64(s[0]), uint32(s[1]), uintptr(s[2])
			base := uintptr(unsafe.Pointer(&mem.Buffer[0]))

			offset := uint32(addr - base)
			res := mem.Wait32(offset, exp, timeout, func(mem *wasm.MemoryInstance, offset uint32) uint32 {
				addr := unsafe.Add(unsafe.Pointer(&mem.Buffer[0]), offset)
				return atomic.LoadUint32((*uint32)(addr))
			})
			s[0] = res
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeMemoryWait64:
			mod := c.callerModuleInstance()
			mem := mod.MemoryInstance
			if !mem.Shared {
				panic(wasmruntime.ErrRuntimeExpectedSharedMemory)
			}

			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			timeout, exp, addr := int64(s[0]), uint64(s[1]), uintptr(s[2])
			base := uintptr(unsafe.Pointer(&mem.Buffer[0]))

			offset := uint32(addr - base)
			res := mem.Wait64(offset, exp, timeout, func(mem *wasm.MemoryInstance, offset uint32) uint64 {
				addr := unsafe.Add(unsafe.Pointer(&mem.Buffer[0]), offset)
				return atomic.LoadUint64((*uint64)(addr))
			})
			s[0] = uint64(res)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeMemoryNotify:
			mod := c.callerModuleInstance()
			mem := mod.MemoryInstance

			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			count, addr := uint32(s[0]), s[1]
			offset := uint32(uintptr(addr) - uintptr(unsafe.Pointer(&mem.Buffer[0])))
			res := mem.Notify(offset, count)
			s[0] = uint64(res)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		default:
			if trap, ok := runtimeTrapFromExitCode(ec & wazevoapi.ExitCodeMask); ok {
				panic(trap)
			}
			panic("BUG")
		}
	}
}

func runtimeTrapFromExitCode(exitCode wazevoapi.ExitCode) (*wasmruntime.Error, bool) {
	switch exitCode {
	case wazevoapi.ExitCodeUnreachable:
		return wasmruntime.ErrRuntimeUnreachable, true
	case wazevoapi.ExitCodeMemoryOutOfBounds:
		return wasmruntime.ErrRuntimeOutOfBoundsMemoryAccess, true
	case wazevoapi.ExitCodeTableOutOfBounds, wazevoapi.ExitCodeIndirectCallNullPointer:
		return wasmruntime.ErrRuntimeInvalidTableAccess, true
	case wazevoapi.ExitCodeIndirectCallTypeMismatch:
		return wasmruntime.ErrRuntimeIndirectCallTypeMismatch, true
	case wazevoapi.ExitCodeIntegerOverflow:
		return wasmruntime.ErrRuntimeIntegerOverflow, true
	case wazevoapi.ExitCodeIntegerDivisionByZero:
		return wasmruntime.ErrRuntimeIntegerDivideByZero, true
	case wazevoapi.ExitCodeInvalidConversionToInteger:
		return wasmruntime.ErrRuntimeInvalidConversionToInteger, true
	case wazevoapi.ExitCodeUnalignedAtomic:
		return wasmruntime.ErrRuntimeUnalignedAtomic, true
	case wazevoapi.ExitCodeFuelExhausted:
		return wasmruntime.ErrRuntimeFuelExhausted, true
	case wazevoapi.ExitCodePolicyDenied:
		return wasmruntime.ErrRuntimePolicyDenied, true
	case wazevoapi.ExitCodeMemoryFault:
		return wasmruntime.ErrRuntimeMemoryFault, true
	default:
		return nil, false
	}
}

func notifyTrapObserver(ctx context.Context, mod api.Module, err error) {
	if err == nil {
		return
	}
	observer := experimental.GetTrapObserver(ctx)
	if observer == nil {
		return
	}
	cause, ok := experimental.TrapCauseOf(err)
	if !ok {
		return
	}
	observer.ObserveTrap(ctx, experimental.TrapObservation{
		Module: mod,
		Cause:  cause,
		Err:    err,
	})
}

func notifyHostCallPolicyObserver(ctx context.Context, mod api.Module, hostFunction api.FunctionDefinition, allowed bool) {
	observer := experimental.GetHostCallPolicyObserver(ctx)
	if observer == nil {
		return
	}
	event := experimental.HostCallPolicyEventDenied
	if allowed {
		event = experimental.HostCallPolicyEventAllowed
	}
	observer.ObserveHostCallPolicy(ctx, experimental.HostCallPolicyObservation{
		Module:       mod,
		HostFunction: hostFunction,
		Event:        event,
	})
}

func notifyFuelObserver(ctx, fallbackCtx context.Context, observer experimental.FuelObserver, observation experimental.FuelObservation) {
	observer, observeCtx := resolveFuelObserver(ctx, fallbackCtx, observer)
	if observer == nil {
		return
	}
	observer.ObserveFuel(observeCtx, observation)
}

func resolveFuelObserver(ctx, fallbackCtx context.Context, fallback experimental.FuelObserver) (experimental.FuelObserver, context.Context) {
	if observer := experimental.GetFuelObserver(ctx); observer != nil {
		return observer, ctx
	}
	if fallback != nil {
		return fallback, fallbackCtx
	}
	return nil, nil
}

func selectFuelObserver(ctx context.Context, fallback experimental.FuelObserver) experimental.FuelObserver {
	if observer := experimental.GetFuelObserver(ctx); observer != nil {
		return observer
	}
	return fallback
}

func currentFuelBudget(initialFuel, fuelAdded int64) int64 {
	return initialFuel + fuelAdded
}

func notifyYieldObserver(ctx, fallbackCtx context.Context, observer experimental.YieldObserver, mod api.Module, event experimental.YieldEvent, yieldCount uint64, expectedHostResults int, clock experimental.TimeProvider, yieldedAtNanos int64) {
	observer, observeCtx := resolveYieldObserver(ctx, fallbackCtx, observer)
	if observer == nil {
		return
	}
	observation := experimental.YieldObservation{
		Module:              mod,
		Event:               event,
		YieldCount:          yieldCount,
		ExpectedHostResults: expectedHostResults,
	}
	if event != experimental.YieldEventYielded && clock != nil && yieldedAtNanos >= 0 {
		if now := clock.Nanotime(); now >= yieldedAtNanos {
			observation.SuspendedNanos = now - yieldedAtNanos
		}
	}
	observer.ObserveYield(observeCtx, observation)
}

func resolveYieldObserver(ctx, fallbackCtx context.Context, fallback experimental.YieldObserver) (experimental.YieldObserver, context.Context) {
	if observer := experimental.GetYieldObserver(ctx); observer != nil {
		return observer, ctx
	}
	if fallback != nil {
		return fallback, fallbackCtx
	}
	return nil, nil
}

func selectYieldObserver(ctx context.Context, fallback experimental.YieldObserver) experimental.YieldObserver {
	if observer := experimental.GetYieldObserver(ctx); observer != nil {
		return observer
	}
	return fallback
}

func effectiveYieldClock(ctx context.Context, fallback experimental.TimeProvider) experimental.TimeProvider {
	if provider := experimental.GetTimeProvider(ctx); provider != nil {
		return provider
	}
	return fallback
}

func yieldNanotime(provider experimental.TimeProvider) int64 {
	if provider == nil {
		return -1
	}
	return provider.Nanotime()
}

func (c *callEngine) callerModuleInstance() *wasm.ModuleInstance {
	return moduleInstanceFromOpaquePtr(c.execCtx.callerModuleContextPtr)
}

func (c *callEngine) allowHostCall(ctx context.Context, caller api.Module, index int) bool {
	policy := experimental.GetHostCallPolicy(ctx)
	if policy == nil {
		return true
	}
	def := c.hostFunctionDefinition(index)
	allowed := policy.AllowHostCall(ctx, caller, def)
	notifyHostCallPolicyObserver(ctx, caller, def, allowed)
	return allowed
}

func (c *callEngine) hostFunctionDefinition(index int) api.FunctionDefinition {
	hostModule := hostModuleFromOpaque(c.execCtx.goFunctionCallCalleeModuleContextOpaque)
	return hostModule.FunctionDefinition(wasm.Index(index))
}

func (c *callEngine) withHostYielder(ctx context.Context, caller api.Module, index int) context.Context {
	if ctx.Value(expctxkeys.EnableYielderKey{}) == nil {
		return ctx
	}
	return context.WithValue(ctx, expctxkeys.YielderKey{}, &compilerYielder{
		ce:           c,
		ctx:          ctx,
		caller:       caller,
		hostFunction: c.hostFunctionDefinition(index),
	})
}

func hostCallContext(ctx context.Context, caller *wasm.ModuleInstance) context.Context {
	return wasm.ApplyCallContextDefaults(ctx, caller)
}

const callStackCeiling = uintptr(50000000) // in uint64 (8 bytes) == 400000000 bytes in total == 400mb.

func (c *callEngine) growStackWithGuarded() (newSP uintptr, newFP uintptr, err error) {
	if wazevoapi.StackGuardCheckEnabled {
		wazevoapi.CheckStackGuardPage(c.stack)
	}
	newSP, newFP, err = c.growStack()
	if err != nil {
		return
	}
	if wazevoapi.StackGuardCheckEnabled {
		c.execCtx.stackBottomPtr = &c.stack[wazevoapi.StackGuardCheckGuardPageSize]
	}
	return
}

// growStack grows the stack, and returns the new stack pointer.
func (c *callEngine) growStack() (newSP, newFP uintptr, err error) {
	currentLen := uintptr(len(c.stack))
	if callStackCeiling < currentLen {
		err = wasmruntime.ErrRuntimeStackOverflow
		return
	}

	newLen := 2*currentLen + c.execCtx.stackGrowRequiredSize + 16 // Stack might be aligned to 16 bytes, so add 16 bytes just in case.
	newSP, newFP, c.stackTop, c.stack = c.cloneStack(newLen)
	c.execCtx.stackBottomPtr = &c.stack[0]
	return
}

func (c *callEngine) cloneStack(l uintptr) (newSP, newFP, newTop uintptr, newStack []byte) {
	newStack = make([]byte, l)

	relSp := c.stackTop - uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall))
	relFp := c.stackTop - c.execCtx.framePointerBeforeGoCall

	// Copy the existing contents in the previous Go-allocated stack into the new one.
	var prevStackAligned, newStackAligned []byte
	{
		//nolint:staticcheck
		sh := (*reflect.SliceHeader)(unsafe.Pointer(&prevStackAligned))
		sh.Data = c.stackTop - relSp
		sh.Len = int(relSp)
		sh.Cap = int(relSp)
	}
	newTop = alignedStackTop(newStack)
	{
		newSP = newTop - relSp
		newFP = newTop - relFp
		//nolint:staticcheck
		sh := (*reflect.SliceHeader)(unsafe.Pointer(&newStackAligned))
		sh.Data = newSP
		sh.Len = int(relSp)
		sh.Cap = int(relSp)
	}
	copy(newStackAligned, prevStackAligned)
	return
}

func (c *callEngine) stackIterator(onHostCall bool) experimental.StackIterator {
	c.stackIteratorImpl.reset(c, onHostCall)
	return &c.stackIteratorImpl
}

// stackIterator implements experimental.StackIterator.
type stackIterator struct {
	retAddrs      []uintptr
	retAddrCursor int
	eng           *engine
	pc            uint64

	currentDef *wasm.FunctionDefinition
}

func (si *stackIterator) reset(c *callEngine, onHostCall bool) {
	if onHostCall {
		si.retAddrs = append(si.retAddrs[:0], uintptr(unsafe.Pointer(c.execCtx.goCallReturnAddress)))
	} else {
		si.retAddrs = si.retAddrs[:0]
	}
	si.retAddrs = unwindStack(uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall, c.stackTop, si.retAddrs)
	si.retAddrs = si.retAddrs[:len(si.retAddrs)-1] // the last return addr is the trampoline, so we skip it.
	si.retAddrCursor = 0
	si.eng = c.parent.parent.parent
}

// Next implements the same method as documented on experimental.StackIterator.
func (si *stackIterator) Next() bool {
	if si.retAddrCursor >= len(si.retAddrs) {
		return false
	}

	addr := si.retAddrs[si.retAddrCursor]
	cm := si.eng.compiledModuleOfAddr(addr)
	if cm != nil {
		index := cm.functionIndexOf(addr)
		def := cm.module.FunctionDefinition(cm.module.ImportFunctionCount + index)
		si.currentDef = def
		si.retAddrCursor++
		si.pc = uint64(addr)
		return true
	}
	return false
}

// ProgramCounter implements the same method as documented on experimental.StackIterator.
func (si *stackIterator) ProgramCounter() experimental.ProgramCounter {
	return experimental.ProgramCounter(si.pc)
}

// Function implements the same method as documented on experimental.StackIterator.
func (si *stackIterator) Function() experimental.InternalFunction {
	return si
}

// Definition implements the same method as documented on experimental.InternalFunction.
func (si *stackIterator) Definition() api.FunctionDefinition {
	return si.currentDef
}

// SourceOffsetForPC implements the same method as documented on experimental.InternalFunction.
func (si *stackIterator) SourceOffsetForPC(pc experimental.ProgramCounter) uint64 {
	upc := uintptr(pc)
	cm := si.eng.compiledModuleOfAddr(upc)
	return cm.getSourceOffset(upc)
}

// snapshot implements experimental.Snapshot
type snapshot struct {
	sp, fp, top    uintptr
	returnAddress  *byte
	stack          []byte
	savedRegisters [64][2]uint64
	ret            []uint64
	c              *callEngine
}

// Snapshot implements the same method as documented on experimental.Snapshotter.
func (c *callEngine) Snapshot() experimental.Snapshot {
	returnAddress := c.execCtx.goCallReturnAddress
	oldTop, oldSp := c.stackTop, uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall))
	newSP, newFP, newTop, newStack := c.cloneStack(uintptr(len(c.stack)) + 16)
	adjustClonedStack(oldSp, oldTop, newSP, newFP, newTop)
	return &snapshot{
		sp:             newSP,
		fp:             newFP,
		top:            newTop,
		savedRegisters: c.execCtx.savedRegisters,
		returnAddress:  returnAddress,
		stack:          newStack,
		c:              c,
	}
}

// Restore implements the same method as documented on experimental.Snapshot.
func (s *snapshot) Restore(ret []uint64) {
	s.ret = ret
	panic(s)
}

func (s *snapshot) doRestore() {
	spp := *(**uint64)(unsafe.Pointer(&s.sp))
	view := goCallStackView(spp)
	copy(view, s.ret)

	c := s.c
	c.stack = s.stack
	c.stackTop = s.top
	ec := &c.execCtx
	ec.stackBottomPtr = &c.stack[0]
	ec.stackPointerBeforeGoCall = spp
	ec.framePointerBeforeGoCall = s.fp
	ec.goCallReturnAddress = s.returnAddress
	ec.savedRegisters = s.savedRegisters
}

// Error implements the same method on error.
func (s *snapshot) Error() string {
	return "unhandled snapshot restore, this generally indicates restore was called from a different " +
		"exported function invocation than snapshot"
}

func snapshotRecoverFn(c *callEngine) {
	if r := recover(); r != nil {
		if s, ok := r.(*snapshot); ok && s.c == c {
			s.doRestore()
		} else {
			panic(r)
		}
	}
}

// yieldRecoverFn recovers yield signals from host function calls in the compiler engine.
func yieldRecoverFn(c *callEngine) {
	if r := recover(); r != nil {
		if _, ok := r.(*compilerYieldSignal); ok {
			// Re-panic so callWithStack catches it.
			panic(r)
		} else if s, ok := r.(*snapshot); ok && s.c == c {
			s.doRestore()
		} else {
			panic(r)
		}
	}
}

func (c *callEngine) currentHostFunctionResultCount() int {
	index := wasm.Index(wazevoapi.GoFunctionIndexFromExitCode(c.execCtx.exitCode))
	hostModule := hostModuleFromOpaque(c.execCtx.goFunctionCallCalleeModuleContextOpaque)
	typeIndex := hostModule.FunctionSection[index]
	return hostModule.TypeSection[typeIndex].ResultNumInUint64
}

// --- Async Yield/Resume for compiler engine ---

// compilerYieldSignal is the panic sentinel for async yield in the compiler engine.
type compilerYieldSignal struct {
	ce *callEngine
}

func (y *compilerYieldSignal) Error() string {
	return "unhandled yield signal in compiler engine"
}

// compilerYielder implements experimental.Yielder for the compiler engine.
type compilerYielder struct {
	ce           *callEngine
	ctx          context.Context
	caller       api.Module
	hostFunction api.FunctionDefinition
}

// Yield implements experimental.Yielder.
func (y *compilerYielder) Yield() {
	if policy := experimental.GetYieldPolicy(y.ctx); policy != nil && !policy.AllowYield(y.ctx, y.caller, y.hostFunction) {
		panic(wasmruntime.ErrRuntimePolicyDenied)
	}
	panic(&compilerYieldSignal{ce: y.ce})
}

// compilerResumer implements experimental.Resumer for the compiler engine.
type compilerResumer struct {
	sp, fp, top    uintptr
	returnAddress  *byte
	stack          []byte
	savedRegisters [64][2]uint64

	ce     *callEngine
	module *wasm.ModuleInstance

	// paramResultStack keeps the original paramResultStack alive so that
	// the native code's saved paramResultPtr remains valid after resume.
	// The native epilogue writes final results here.
	paramResultStack []uint64

	// Fuel state to preserve across yield/resume.
	fuelCtrl        experimental.FuelController
	initialFuel     int64
	fuelAdded       int64
	fuelObserver    experimental.FuelObserver
	fuelObserverCtx context.Context

	lifecycle yieldstate.Lifecycle

	expectedHostResults int
	yieldCount          uint64
	yieldObserver       experimental.YieldObserver
	yieldObserverCtx    context.Context
	yieldClock          experimental.TimeProvider
	yieldedAtNanos      int64
}

// Resume implements experimental.Resumer for the compiler engine.
func (r *compilerResumer) Resume(ctx context.Context, hostResults []uint64) (results []uint64, err error) {
	if ctx == nil {
		return nil, errors.New("cannot resume: context is nil")
	}
	if len(hostResults) != r.expectedHostResults {
		return nil, fmt.Errorf("cannot resume: expected %d host results, but got %d", r.expectedHostResults, len(hostResults))
	}
	if err := r.lifecycle.BeginResume(); err != nil {
		return nil, err
	}
	defer r.lifecycle.FinishResume()

	c := r.ce
	if !c.yieldState.CompareAndSwap(compilerYieldStateSuspended, compilerYieldStateResuming) {
		panic("BUG: concurrent or invalid Resume call on compilerResumer")
	}
	r.module.BeginResume()
	yieldedAgain := false
	defer func() {
		r.module.FinishResume(yieldedAgain)
	}()
	if err := r.module.FailIfClosed(); err != nil {
		r.stack = nil
		r.paramResultStack = nil
		r.ce.yieldState.Store(compilerYieldStateIdle)
		return nil, err
	}
	notifyYieldObserver(ctx, r.yieldObserverCtx, r.yieldObserver, r.module, experimental.YieldEventResumed, r.yieldCount, r.expectedHostResults, r.yieldClock, r.yieldedAtNanos)

	// Restore the cloned native stack into the call engine.
	spp := *(**uint64)(unsafe.Pointer(&r.sp))
	view := goCallStackView(spp)
	copy(view, hostResults)
	if def, lsn := c.addFrame(wasmdebug.NewErrorBuilder(), uintptr(unsafe.Pointer(r.returnAddress))); lsn != nil {
		lsn.After(ctx, r.module, def, hostResults)
	}

	c.stack = r.stack
	c.stackTop = r.top
	ec := &c.execCtx
	ec.stackBottomPtr = &c.stack[0]
	ec.stackPointerBeforeGoCall = spp
	ec.framePointerBeforeGoCall = r.fp
	ec.goCallReturnAddress = r.returnAddress
	ec.savedRegisters = r.savedRegisters

	// Restore fuel state.
	fuelCtrl := r.fuelCtrl
	initialFuel := r.initialFuel
	fuelAdded := r.fuelAdded
	fuelObserver := selectFuelObserver(ctx, r.fuelObserver)
	fuelObserverCtx := r.fuelObserverCtx
	if fuelObserverCtx == nil {
		fuelObserverCtx = ctx
	}
	if initialFuel > 0 {
		ctx = context.WithValue(ctx, expctxkeys.FuelAccessorKey{}, &expctxkeys.FuelAccessor{Ptr: &c.execCtx.fuel, Module: r.module, Added: &fuelAdded})
		if experimental.GetFuelObserver(ctx) == nil && fuelObserver != nil {
			ctx = context.WithValue(ctx, expctxkeys.FuelObserverKey{}, fuelObserver)
		}
	}

	// Preserve yield and snapshot enablement in the resumed context.
	snapshotEnabled := ctx.Value(expctxkeys.EnableSnapshotterKey{}) != nil
	if snapshotEnabled {
		ctx = context.WithValue(ctx, expctxkeys.SnapshotterKey{}, c)
	}
	yieldEnabled := ctx.Value(expctxkeys.EnableYielderKey{}) != nil

	m := r.module

	defer func() {
		rec := recover()
		if s, ok := rec.(*snapshot); ok {
			panic(s)
		}

		// Check for another yield.
		if ys, ok := rec.(*compilerYieldSignal); ok && ys.ce == c {
			retAddr := c.execCtx.goCallReturnAddress
			oldTop, oldSp := c.stackTop, uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall))
			newSP, newFP, newTop, newStack := c.cloneStack(uintptr(len(c.stack)) + 16)
			adjustClonedStack(oldSp, oldTop, newSP, newFP, newTop)

			newResumer := &compilerResumer{
				sp:                  newSP,
				fp:                  newFP,
				top:                 newTop,
				savedRegisters:      c.execCtx.savedRegisters,
				returnAddress:       retAddr,
				stack:               newStack,
				ce:                  c,
				module:              m,
				paramResultStack:    r.paramResultStack,
				fuelCtrl:            fuelCtrl,
				initialFuel:         initialFuel,
				fuelAdded:           fuelAdded,
				fuelObserver:        fuelObserver,
				fuelObserverCtx:     ctx,
				expectedHostResults: c.currentHostFunctionResultCount(),
				yieldCount:          r.yieldCount + 1,
				yieldObserver:       selectYieldObserver(ctx, r.yieldObserver),
				yieldObserverCtx:    ctx,
				yieldClock:          effectiveYieldClock(ctx, r.yieldClock),
				yieldedAtNanos:      yieldNanotime(effectiveYieldClock(ctx, r.yieldClock)),
			}
			c.yieldState.Store(compilerYieldStateSuspended)
			yieldedAgain = true
			notifyYieldObserver(ctx, ctx, newResumer.yieldObserver, m, experimental.YieldEventYielded, newResumer.yieldCount, newResumer.expectedHostResults, nil, 0)
			err = experimental.NewYieldError(newResumer)
			results = nil
			return
		}

		if rec != nil {
			var listeners []struct {
				def api.FunctionDefinition
				lsn experimental.FunctionListener
			}
			builder := wasmdebug.NewErrorBuilder()
			def, lsn := c.addFrame(builder, uintptr(unsafe.Pointer(c.execCtx.goCallReturnAddress)))
			if lsn != nil {
				listeners = append(listeners, struct {
					def api.FunctionDefinition
					lsn experimental.FunctionListener
				}{def, lsn})
			}
			err = builder.FromRecovered(rec)
			for _, l := range listeners {
				l.lsn.Abort(ctx, m, l.def, err)
			}
		} else {
			if err != wasmruntime.ErrRuntimeStackOverflow {
				err = m.FailIfClosed()
			}
		}

		if err != nil && initialFuel > 0 {
			event := experimental.FuelEventConsumed
			if errors.Is(err, wasmruntime.ErrRuntimeFuelExhausted) {
				event = experimental.FuelEventExhausted
			}
			budget := currentFuelBudget(initialFuel, fuelAdded)
			notifyFuelObserver(ctx, fuelObserverCtx, fuelObserver, experimental.FuelObservation{
				Module:    m,
				Event:     event,
				Budget:    budget,
				Consumed:  budget - c.execCtx.fuel,
				Remaining: c.execCtx.fuel,
			})
		}
		if err != nil && fuelCtrl != nil && initialFuel > 0 {
			consumed := currentFuelBudget(initialFuel, fuelAdded) - c.execCtx.fuel
			if consumed > 0 {
				fuelCtrl.Consumed(consumed)
			}
		}
		notifyTrapObserver(ctx, m, err)
		c.execCtx.exitCode = wazevoapi.ExitCodeOK
		c.yieldState.Store(compilerYieldStateIdle)
	}()

	// Re-enter the native code from the saved return address.
	c.execCtx.exitCode = wazevoapi.ExitCodeOK
	afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
		uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)

	// Process exit codes in a loop, just like callWithStack.
	for {
		switch ec := c.execCtx.exitCode; ec & wazevoapi.ExitCodeMask {
		case wazevoapi.ExitCodeOK:
			if initialFuel > 0 {
				budget := currentFuelBudget(initialFuel, fuelAdded)
				notifyFuelObserver(ctx, fuelObserverCtx, fuelObserver, experimental.FuelObservation{
					Module:    m,
					Event:     experimental.FuelEventConsumed,
					Budget:    budget,
					Consumed:  budget - c.execCtx.fuel,
					Remaining: c.execCtx.fuel,
				})
			}
			if fuelCtrl != nil && initialFuel > 0 {
				consumed := currentFuelBudget(initialFuel, fuelAdded) - c.execCtx.fuel
				if consumed > 0 {
					fuelCtrl.Consumed(consumed)
				}
			}
			c.yieldState.Store(compilerYieldStateIdle)
			// Read results from the paramResultStack where the native epilogue
			// wrote them (via the saved paramResultPtr).
			numResults := c.numberOfResults
			if numResults > 0 {
				results = make([]uint64, numResults)
				copy(results, r.paramResultStack[:numResults])
			}
			return results, nil
		case wazevoapi.ExitCodeGrowStack:
			oldStack := c.stack
			var newsp, newfp uintptr
			newsp, newfp, err = c.growStack()
			if err != nil {
				return nil, err
			}
			adjustClonedStack(uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.stackTop, newsp, newfp, c.stackTop)
			runtime.KeepAlive(oldStack)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr, newsp, newfp)
		case wazevoapi.ExitCodeCallGoFunction:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			callerModule := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, callerModule)
			if !c.allowHostCall(callCtx, callerModule, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			hostCtx := c.withHostYielder(callCtx, callerModule, index)
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, goCallStackView(c.execCtx.stackPointerBeforeGoCall))
			}()
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoFunctionWithListener:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			callerModule := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, callerModule)
			if !c.allowHostCall(callCtx, callerModule, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			listeners := hostModuleListenersSliceFromOpaque(c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			listener := listeners[index]
			def := c.hostFunctionDefinition(index)
			listener.Before(callCtx, callerModule, def, s, c.stackIterator(true))
			hostCtx := c.withHostYielder(callCtx, callerModule, index)
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, s)
			}()
			listener.After(callCtx, callerModule, def, s)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoModuleFunction:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			mod := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, mod)
			if !c.allowHostCall(callCtx, mod, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoModuleFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			hostCtx := c.withHostYielder(callCtx, mod, index)
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, mod, goCallStackView(c.execCtx.stackPointerBeforeGoCall))
			}()
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallGoModuleFunctionWithListener:
			index := wazevoapi.GoFunctionIndexFromExitCode(ec)
			callerModule := c.callerModuleInstance()
			callCtx := hostCallContext(ctx, callerModule)
			if !c.allowHostCall(callCtx, callerModule, index) {
				c.execCtx.exitCode = wazevoapi.ExitCodePolicyDenied
				continue
			}
			f := hostModuleGoFuncFromOpaque[api.GoModuleFunction](index, c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			listeners := hostModuleListenersSliceFromOpaque(c.execCtx.goFunctionCallCalleeModuleContextOpaque)
			s := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			listener := listeners[index]
			def := c.hostFunctionDefinition(index)
			listener.Before(callCtx, callerModule, def, s, c.stackIterator(true))
			hostCtx := c.withHostYielder(callCtx, callerModule, index)
			func() {
				if yieldEnabled {
					defer yieldRecoverFn(c)
				} else if snapshotEnabled {
					defer snapshotRecoverFn(c)
				}
				f.Call(hostCtx, callerModule, s)
			}()
			listener.After(callCtx, callerModule, def, s)
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallListenerBefore:
			stack := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			index := wasm.Index(stack[0])
			mod := c.callerModuleInstance()
			listener := mod.Engine.(*moduleEngine).listeners[index]
			def := mod.Source.FunctionDefinition(index + mod.Source.ImportFunctionCount)
			listener.Before(ctx, mod, def, stack[1:], c.stackIterator(false))
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCallListenerAfter:
			stack := goCallStackView(c.execCtx.stackPointerBeforeGoCall)
			index := wasm.Index(stack[0])
			mod := c.callerModuleInstance()
			listener := mod.Engine.(*moduleEngine).listeners[index]
			def := mod.Source.FunctionDefinition(index + mod.Source.ImportFunctionCount)
			listener.After(ctx, mod, def, stack[1:])
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		case wazevoapi.ExitCodeCheckModuleExitCode:
			if err := m.FailIfClosed(); err != nil {
				panic(err)
			}
			c.execCtx.exitCode = wazevoapi.ExitCodeOK
			afterGoFunctionCallEntrypoint(c.execCtx.goCallReturnAddress, c.execCtxPtr,
				uintptr(unsafe.Pointer(c.execCtx.stackPointerBeforeGoCall)), c.execCtx.framePointerBeforeGoCall)
		default:
			if trap, ok := runtimeTrapFromExitCode(ec & wazevoapi.ExitCodeMask); ok {
				panic(trap)
			}
			panic("BUG")
		}
	}
}

// Cancel implements experimental.Resumer.
func (r *compilerResumer) Cancel() {
	if r.lifecycle.Cancel() {
		r.stack = nil
		r.ce.yieldState.Store(compilerYieldStateIdle)
		r.module.CancelSuspend()
		notifyYieldObserver(r.yieldObserverCtx, r.yieldObserverCtx, r.yieldObserver, r.module, experimental.YieldEventCancelled, r.yieldCount, r.expectedHostResults, r.yieldClock, r.yieldedAtNanos)
	}
}

// compilerYieldState constants.
const (
	compilerYieldStateIdle      int32 = 0
	compilerYieldStateSuspended int32 = 1
	compilerYieldStateResuming  int32 = 2
)
