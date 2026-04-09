package yieldstate

import (
	"errors"
	"fmt"
	"sync/atomic"
)

var (
	ErrResumerCancelled  = errors.New("cannot resume: resumer has been cancelled")
	ErrResumerInProgress = errors.New("cannot resume: resumer is already being resumed")
	ErrResumerUsed       = errors.New("cannot resume: resumer has already been used")
)

const (
	stateSuspended uint32 = iota
	stateResuming
	stateSpent
	stateCancelled
)

// Lifecycle tracks the valid state transitions for a yielded resumer.
//
// Zero-value Lifecycle starts in the suspended state.
type Lifecycle struct {
	state atomic.Uint32
}

// BeginResume transitions a suspended resumer into resuming.
func (l *Lifecycle) BeginResume() error {
	for {
		switch state := l.state.Load(); state {
		case stateSuspended:
			if l.state.CompareAndSwap(stateSuspended, stateResuming) {
				return nil
			}
		case stateResuming:
			return ErrResumerInProgress
		case stateSpent:
			return ErrResumerUsed
		case stateCancelled:
			return ErrResumerCancelled
		default:
			panic(fmt.Sprintf("BUG: unknown resumer lifecycle state %d", state))
		}
	}
}

// FinishResume marks a resumer as spent after Resume has started.
func (l *Lifecycle) FinishResume() {
	if !l.state.CompareAndSwap(stateResuming, stateSpent) {
		panic(fmt.Sprintf("BUG: invalid resumer lifecycle transition from %d to spent", l.state.Load()))
	}
}

// Cancel transitions a suspended resumer into cancelled.
//
// Returns true only when the caller performed the cancellation.
func (l *Lifecycle) Cancel() bool {
	for {
		switch state := l.state.Load(); state {
		case stateSuspended:
			if l.state.CompareAndSwap(stateSuspended, stateCancelled) {
				return true
			}
		case stateResuming, stateSpent, stateCancelled:
			return false
		default:
			panic(fmt.Sprintf("BUG: unknown resumer lifecycle state %d", state))
		}
	}
}
