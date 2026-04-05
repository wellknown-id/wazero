package wasm

import (
	"context"
	"fmt"
	"testing"

	"github.com/tetratelabs/wazero/internal/testing/require"
)

func TestModuleInstance_String(t *testing.T) {
	s := newStore()

	tests := []struct {
		name, moduleName, expected string
	}{
		{
			name:       "empty",
			moduleName: "",
			expected:   "Module[]",
		},
		{
			name:       "not empty",
			moduleName: "math",
			expected:   "Module[math]",
		},
	}

	for _, tt := range tests {
		tc := tt

		t.Run(tc.name, func(t *testing.T) {
			// Ensure paths that can create the host module can see the name.
			m, err := s.Instantiate(testCtx, &Module{}, tc.moduleName, nil)
			defer m.Close(testCtx) //nolint

			require.NoError(t, err)
			require.Equal(t, tc.expected, m.String())

			if name := m.Name(); name != "" {
				sm := s.Module(m.Name())
				if sm != nil {
					require.Equal(t, tc.expected, s.Module(m.Name()).String())
				} else {
					require.Zero(t, len(m.Name()))
				}
			}
		})
	}
}

func TestModuleInstance_CallDynamic(t *testing.T) {
	s := newStore()

	tests := []struct {
		name           string
		closer         func(context.Context, *ModuleInstance) error
		expectedClosed uint64
	}{
		{
			name: "Close()",
			closer: func(ctx context.Context, m *ModuleInstance) error {
				return m.Close(ctx)
			},
			expectedClosed: uint64(1),
		},
		{
			name: "CloseWithExitCode(255)",
			closer: func(ctx context.Context, m *ModuleInstance) error {
				return m.CloseWithExitCode(ctx, 255)
			},
			expectedClosed: uint64(255)<<32 + 1,
		},
	}

	for _, tt := range tests {
		tc := tt
		t.Run(fmt.Sprintf("%s calls ns.CloseWithExitCode(module.name))", tc.name), func(t *testing.T) {
			moduleName := t.Name()
			m, err := s.Instantiate(testCtx, &Module{}, moduleName, nil)
			require.NoError(t, err)

			// We use side effects to see if Close called ns.CloseWithExitCode (without repeating store_test.go).
			// One side effect of ns.CloseWithExitCode is that the moduleName can no longer be looked up.
			require.Equal(t, s.Module(moduleName), m)

			// Closing should not err.
			require.NoError(t, tc.closer(testCtx, m))

			require.Equal(t, tc.expectedClosed, m.Closed.Load())

			// Verify our intended side-effect
			require.Nil(t, s.Module(moduleName))

			// Verify no error closing again.
			require.NoError(t, tc.closer(testCtx, m))
		})
	}
}

type mockCloser struct{ called int }

func (m *mockCloser) Close(context.Context) error {
	m.called++
	return nil
}
