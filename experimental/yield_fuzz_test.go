package experimental_test

import (
	"context"
	"fmt"
	"testing"

	"github.com/tetratelabs/wazero/experimental"
	"github.com/tetratelabs/wazero/internal/testing/require"
)

const (
	yieldMalformedNilContext uint8 = iota
	yieldMalformedMissingResults
	yieldMalformedExtraResults
)

func FuzzYieldResumeLifecycle(f *testing.F) {
	f.Add(uint8(0), []byte{yieldMalformedNilContext}, []byte{yieldMalformedMissingResults})
	f.Add(uint8(1), []byte{yieldMalformedMissingResults, yieldMalformedExtraResults}, []byte{yieldMalformedNilContext})
	f.Add(uint8(0), []byte{yieldMalformedExtraResults, yieldMalformedNilContext}, []byte{yieldMalformedExtraResults, yieldMalformedMissingResults})

	f.Fuzz(func(t *testing.T, mode uint8, firstOps, secondOps []byte) {
		ec := engineConfigs()[int(mode)%len(engineConfigs())]
		mod, rt, ctx := setupYieldTest(t, ec.cfg)
		defer rt.Close(ctx)

		_, err := mod.ExportedFunction("run_twice").Call(experimental.WithYielder(ctx))
		firstResumer := requireYieldError(t, err).Resumer()

		fuzzMalformedResumeAttempts(t, firstResumer, firstOps, 1)

		_, err = firstResumer.Resume(experimental.WithYielder(ctx), []uint64{40})
		secondResumer := requireYieldError(t, err).Resumer()

		_, err = firstResumer.Resume(experimental.WithYielder(ctx), []uint64{1})
		require.EqualError(t, err, "cannot resume: resumer has already been used")

		fuzzMalformedResumeAttempts(t, secondResumer, secondOps, 1)

		results, err := secondResumer.Resume(experimental.WithYielder(ctx), []uint64{2})
		require.NoError(t, err)
		require.Equal(t, []uint64{42}, results)

		_, err = secondResumer.Resume(experimental.WithYielder(ctx), []uint64{2})
		require.EqualError(t, err, "cannot resume: resumer has already been used")
	})
}

func fuzzMalformedResumeAttempts(t *testing.T, resumer experimental.Resumer, ops []byte, expectedHostResults int) {
	t.Helper()

	for _, op := range ops {
		switch op % 3 {
		case yieldMalformedNilContext:
			_, err := resumer.Resume(nil, make([]uint64, expectedHostResults))
			require.EqualError(t, err, "cannot resume: context is nil")
		case yieldMalformedMissingResults:
			_, err := resumer.Resume(experimental.WithYielder(context.Background()), nil)
			require.EqualError(t, err, fmt.Sprintf("cannot resume: expected %d host results, but got 0", expectedHostResults))
		case yieldMalformedExtraResults:
			_, err := resumer.Resume(experimental.WithYielder(context.Background()), make([]uint64, expectedHostResults+1))
			require.EqualError(t, err, fmt.Sprintf("cannot resume: expected %d host results, but got %d", expectedHostResults, expectedHostResults+1))
		}
	}
}

func FuzzYieldConcurrentResumeState(f *testing.F) {
	f.Add(uint8(0), uint32(40), uint32(7))
	f.Add(uint8(1), uint32(0), uint32(99))
	f.Add(uint8(0), ^uint32(0), uint32(1))

	type resumeOutcome struct {
		results []uint64
		err     error
	}

	f.Fuzz(func(t *testing.T, mode uint8, firstHostResult, secondHostResult uint32) {
		ec := engineConfigs()[int(mode)%len(engineConfigs())]
		host := &blockingResumeHostFunc{
			t:             t,
			resumeStarted: make(chan struct{}),
			releaseResume: make(chan struct{}),
		}
		mod, rt, ctx := setupYieldTestWithHost(t, ec.cfg, host)
		defer rt.Close(ctx)

		_, err := mod.ExportedFunction("run_twice").Call(experimental.WithYielder(ctx))
		resumer := requireYieldError(t, err).Resumer()

		outcomes := make(chan resumeOutcome, 2)
		go func() {
			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{uint64(firstHostResult)})
			outcomes <- resumeOutcome{results: results, err: err}
		}()

		<-host.resumeStarted

		go func() {
			results, err := resumer.Resume(experimental.WithYielder(ctx), []uint64{uint64(secondHostResult)})
			outcomes <- resumeOutcome{results: results, err: err}
		}()

		second := <-outcomes
		require.EqualError(t, second.err, "cannot resume: resumer is already being resumed")
		require.Nil(t, second.results)

		close(host.releaseResume)
		first := <-outcomes
		require.NoError(t, first.err)
		require.Equal(t, []uint64{uint64(firstHostResult + 2)}, first.results)

		_, err = resumer.Resume(experimental.WithYielder(ctx), []uint64{uint64(firstHostResult)})
		require.EqualError(t, err, "cannot resume: resumer has already been used")
	})
}
