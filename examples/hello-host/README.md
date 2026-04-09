## Hello world via an explicit host import

This example shows how to run a Wasm guest without WASI by providing exactly one
host function: `env.print(ptr, len)`.

The guest module stores `hello world from guest` in linear memory and exports a
`run` function that calls the imported host print function with the string
pointer and length.

The Rust host example lives at `razero/examples/hello_world.rs` and can be run
like this:

```bash
cargo run -p razero --example hello_world
```

Expected output:

```text
hello world from guest
```

## Native packaging status in the Rust port

The Rust port now also has a first **Linux/x86_64** AOT packaging slice for this
guest.

Current building blocks:

- `CompiledModule::emit_relocatable_object()` in `razero-compiler` emits an ELF
  relocatable object plus a Razero metadata sidecar.
- `razero_compiler::linker::link_native_executable(...)` links modules whose
  exported functions fit the current scalar C ABI wrapper surface.
- `razero_compiler::linker::link_hello_host_executable(...)` packages this
  specific `hello-host` guest by wiring the guest's `env.print(ptr, len)` import
  to a tiny generated host stub that prints from guest memory.

This path is intentionally narrow today:

- Linux/x86_64 only
- no WASI
- intended for AOT-linked execution, not interpreter embedding
- `hello-host` packaging is specialized to the current example shape:
  one `env.print(i32, i32)` import, one local memory, and an exported `run()`

The end-to-end proof currently lives in
`razero-compiler/src/linker.rs::link_hello_host_executable_runs_example_guest`,
which links the guest into a native executable and verifies that it prints:

```text
hello world from guest
```
