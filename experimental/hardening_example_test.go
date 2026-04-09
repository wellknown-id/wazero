package experimental_test

import (
	"context"
	"errors"
	"fmt"
	"log"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
)

// Example_runtimeHardeningHooks demonstrates a practical embedder setup:
// guard imports at instantiation time, install runtime-default host-call policy,
// and add per-call yield/trap observers around an async host boundary.
func Example_runtimeHardeningHooks() {
	ctx := context.Background()

	rt := wazero.NewRuntimeWithConfig(ctx,
		wazero.NewRuntimeConfigInterpreter().WithHostCallPolicy(
			experimental.HostCallPolicyFunc(func(_ context.Context, _ api.Module, hostFunction api.FunctionDefinition) bool {
				return hostFunction.DebugName() == "example.async_work"
			}),
		),
	)
	defer rt.Close(ctx)

	_, err := rt.NewHostModuleBuilder("example").
		NewFunctionBuilder().
		WithGoModuleFunction(api.GoModuleFunc(func(ctx context.Context, mod api.Module, stack []uint64) {
			yielder := experimental.GetYielder(ctx)
			if yielder == nil {
				log.Panicln("yield not enabled")
			}
			yielder.Yield()
		}), nil, []api.ValueType{api.ValueTypeI32}).
		Export("async_work").
		Instantiate(ctx)
	if err != nil {
		log.Panicln(err)
	}

	instantiateCtx := experimental.WithImportResolverConfig(ctx, experimental.ImportResolverConfig{
		ACL: experimental.NewImportACL().AllowModules("example"),
	})

	mod, err := rt.InstantiateWithConfig(instantiateCtx, yieldExampleWasm, wazero.NewModuleConfig().WithName("guest"))
	if err != nil {
		log.Panicln(err)
	}

	var policyEvents []string
	hostObserver := experimental.HostCallPolicyObserverFunc(func(_ context.Context, observation experimental.HostCallPolicyObservation) {
		policyEvents = append(policyEvents, fmt.Sprintf("host policy: %s %s", observation.HostFunction.DebugName(), observation.Event))
	})

	var yieldEvents []string
	yieldObserver := experimental.YieldObserverFunc(func(_ context.Context, observation experimental.YieldObservation) {
		yieldEvents = append(yieldEvents, fmt.Sprintf("yield: %s #%d", observation.Event, observation.YieldCount))
	})

	callCtx := experimental.WithYieldObserver(
		experimental.WithHostCallPolicyObserver(
			experimental.WithYielder(ctx),
			hostObserver,
		),
		yieldObserver,
	)

	_, err = mod.ExportedFunction("run").Call(callCtx)
	var yieldErr *experimental.YieldError
	if !errors.As(err, &yieldErr) {
		log.Panicln("expected YieldError, got:", err)
	}

	results, err := yieldErr.Resumer().Resume(
		experimental.WithYieldObserver(experimental.WithYielder(ctx), yieldObserver),
		[]uint64{42},
	)
	if err != nil {
		log.Panicln(err)
	}

	var trapCause experimental.TrapCause
	deniedCtx := experimental.WithTrapObserver(
		experimental.WithYieldPolicy(
			experimental.WithYielder(ctx),
			experimental.YieldPolicyFunc(func(context.Context, api.Module, api.FunctionDefinition) bool { return false }),
		),
		experimental.TrapObserverFunc(func(_ context.Context, observation experimental.TrapObservation) {
			trapCause = observation.Cause
		}),
	)

	_, err = mod.ExportedFunction("run").Call(deniedCtx)
	if err == nil {
		log.Panicln("expected policy denial")
	}
	if trapCause == "" {
		if cause, ok := experimental.TrapCauseOf(err); ok {
			trapCause = cause
		}
	}

	fmt.Println("instantiation ACL: allow only example")
	fmt.Println(policyEvents[0])
	fmt.Println(yieldEvents[0])
	fmt.Println(yieldEvents[1])
	fmt.Println("result:", results[0])
	fmt.Println("trap:", trapCause)
	// Output:
	// instantiation ACL: allow only example
	// host policy: example.async_work allowed
	// yield: yielded #1
	// yield: resumed #1
	// result: 142
	// trap: policy_denied
}
