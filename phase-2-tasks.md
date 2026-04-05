# Phase 2: Deterministic CPU Metering ("Fuel")

## Tasks

- [x] Component 1: Fuel counter in executionContext + offset constant
  - [x] Add `fuel int64` to `executionContext` struct
  - [x] Add `ExecutionContextOffsetFuel` to offsetdata.go
- [x] Component 2: FuelController in experimental package
  - [x] Context key in `internal/expctxkeys/fuel.go`
  - [x] `FuelController` interface + `WithFuelController` / `GetFuelController`
  - [x] `SimpleFuelController` implementation
  - [x] `AggregatingFuelController` implementation
- [x] Component 3: SSA-level fuel injection in compiler frontend
  - [x] Add `fuelEnabled bool` to `Compiler` struct + constructor
  - [x] Implement `insertFuelCheck()` helper (branch-to-exit-block pattern)
  - [x] Inject at function entry (LowerToSSA)
  - [x] Inject at loop back-edge (OpcodeLoop)
- [x] Component 4: Configuration and runtime wiring
  - [x] `WithFuel(int64)` on `RuntimeConfig`
  - [x] Thread `fuelEnabled` through engine → compileModule → frontend
  - [x] Call engine: init fuel from FuelController/config, report consumption
- [x] Component 5: Roadmap update (ExitIfTrue SSA opcode)
- [x] Component 6: Tests and benchmarks
  - [x] FuelController unit tests (11 tests, all pass)
  - [x] Fuel exhaustion integration tests (7 tests, all pass)
  - [x] Fuel overhead benchmarks (5 benchmarks)
  - [x] Full regression suite (all pass)
- [x] Component 7: Fix mock engine signatures (bool → int64)
  - [x] internal/wasm/store_test.go
  - [x] runtime_test.go
  - [x] config_test.go
  - [x] internal/engine/wazevo/engine_test.go
  - [x] internal/engine/interpreter/interpreter_test.go
