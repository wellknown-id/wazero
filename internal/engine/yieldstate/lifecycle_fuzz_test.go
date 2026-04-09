package yieldstate

import (
	"errors"
	"testing"
)

type lifecycleModelState uint8

const (
	modelSuspended lifecycleModelState = iota
	modelResuming
	modelSpent
	modelCancelled
)

func FuzzLifecycleOperationSequence(f *testing.F) {
	f.Add([]byte{0, 1})
	f.Add([]byte{2, 0, 1})
	f.Add([]byte{0, 2, 1, 3})
	f.Add([]byte{1, 0, 1, 2})

	f.Fuzz(func(t *testing.T, ops []byte) {
		var lifecycle Lifecycle
		state := modelSuspended

		for _, op := range ops {
			switch op % 4 {
			case 0:
				err := lifecycle.BeginResume()
				want := expectedBeginResumeErr(state)
				if !errors.Is(err, want) {
					t.Fatalf("BeginResume error = %v, want %v", err, want)
				}
				if want == nil {
					state = modelResuming
				}
			case 1:
				state = applyFinishOperation(t, &lifecycle, state)
			case 2:
				cancelled := lifecycle.Cancel()
				want := state == modelSuspended
				if cancelled != want {
					t.Fatalf("Cancel() = %v, want %v", cancelled, want)
				}
				if cancelled {
					state = modelCancelled
				}
			case 3:
				err := lifecycle.BeginResume()
				want := expectedBeginResumeErr(state)
				if !errors.Is(err, want) {
					t.Fatalf("BeginResume error = %v, want %v", err, want)
				}
				if want == nil {
					state = modelResuming
					state = applyFinishOperation(t, &lifecycle, state)
				}
			}
		}
	})
}

func FuzzLifecycleConcurrentBeginResume(f *testing.F) {
	f.Add(uint8(2))
	f.Add(uint8(5))
	f.Add(uint8(12))

	f.Fuzz(func(t *testing.T, goroutines uint8) {
		count := int(goroutines%16) + 2
		var lifecycle Lifecycle
		results := make(chan error, count)
		start := make(chan struct{})

		for i := 0; i < count; i++ {
			go func() {
				<-start
				results <- lifecycle.BeginResume()
			}()
		}
		close(start)

		successes := 0
		inProgress := 0
		for i := 0; i < count; i++ {
			err := <-results
			switch {
			case err == nil:
				successes++
			case errors.Is(err, ErrResumerInProgress):
				inProgress++
			default:
				t.Fatalf("BeginResume error = %v, want nil or %v", err, ErrResumerInProgress)
			}
		}

		if successes != 1 {
			t.Fatalf("successful BeginResume calls = %d, want 1", successes)
		}
		if inProgress != count-1 {
			t.Fatalf("in-progress BeginResume calls = %d, want %d", inProgress, count-1)
		}

		lifecycle.FinishResume()
		if err := lifecycle.BeginResume(); !errors.Is(err, ErrResumerUsed) {
			t.Fatalf("BeginResume after FinishResume error = %v, want %v", err, ErrResumerUsed)
		}
	})
}

func expectedBeginResumeErr(state lifecycleModelState) error {
	switch state {
	case modelSuspended:
		return nil
	case modelResuming:
		return ErrResumerInProgress
	case modelSpent:
		return ErrResumerUsed
	case modelCancelled:
		return ErrResumerCancelled
	default:
		return errors.New("unknown model state")
	}
}

func applyFinishOperation(t *testing.T, lifecycle *Lifecycle, state lifecycleModelState) lifecycleModelState {
	t.Helper()

	if state == modelResuming {
		lifecycle.FinishResume()
		return modelSpent
	}

	panicked := false
	func() {
		defer func() {
			panicked = recover() != nil
		}()
		lifecycle.FinishResume()
	}()
	if !panicked {
		t.Fatalf("FinishResume should panic from state %d", state)
	}
	return state
}
