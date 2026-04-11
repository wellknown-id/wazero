## Allocation fixtures

This directory keeps guest-side fixtures for memory-allocation experiments in
WebAssembly, such as passing strings and byte buffers across the host/guest
boundary.

While the below examples use strings, they are written in a way that would work
for binary serialization.

* [Rust](rust) - guest built with `cargo build --release --target wasm32-unknown-unknown`
* [TinyGo](tinygo) - guest built with `tinygo build -o X.wasm -scheduler=none --no-debug -target=wasip1 X.go`
* [Zig](zig) - Calls Wasm built with `zig build`

Note: Each of the above languages differ in both terms of exports and runtime
behavior around allocation, because there is no WebAssembly specification for
it. For example, TinyGo exports allocation functions while Rust and Zig don't.
Also, Rust eagerly collects memory before returning from a Wasm function while TinyGo
does not.

The old Go-host walkthrough from the original porting source no longer exists in
this Rust workspace, so these directories should currently be read as fixture
documentation rather than fully runnable host examples.
