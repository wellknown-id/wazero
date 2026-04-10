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

## Supported packaging contract

The frozen Rust AOT packaging ABI now lives in
`razero-compiler/AOT_PACKAGING_ABI.md`.

Key points for this example and future packaging work:

- `.razero-package` stores module metadata plus explicit packaged host-import
  descriptors.
- execution-context offsets, module-context offsets, helper IDs, and the
  documented link-visible symbols are treated as supported ABI.
- the general native-link path remains Linux/ELF first and C ABI first.
- `hello-host` now uses the reusable packaged host-import descriptor path, while
  keeping its concrete host behavior explicit.
- `razero` keeps owning interpreter/runtime embedding and precompiled artifacts;
  `razero-ffi` remains optional.

## Native packaging status in the Rust port

The Rust port now also has a first **Linux/ELF** AOT packaging slice for this
guest.

Current building blocks:

- `CompiledModule::emit_relocatable_object()` in `razero-compiler` emits an ELF
  relocatable object plus a Razero metadata sidecar.
- `razero_compiler::linker::link_native_executable(...)` links modules whose
  exported functions fit the current scalar C ABI wrapper surface.
- `razero_compiler::linker::link_hello_host_executable(...)` packages this
  specific `hello-host` guest by emitting a packaged host descriptor for its
  single explicit `(i32, i32) -> ()` host import and wiring it to an explicit
  generated host stub that prints from guest memory.

This path is intentionally narrow today:

- Linux/x86_64 and Linux/aarch64
- no WASI
- interpreter support remains a required product path; this is an additional
  AOT-linked deployment target, not a replacement
- host ownership stays explicit: the linker/runtime support dispatches through
  declared packaged host descriptors instead of hidden runtime behavior
- runtime-state packaging is still specialized to the current example shape:
  one local memory, active data loading, one explicit `(i32, i32) -> ()` host
  import, and an exported `run()`

The frozen contract for the current Rust AOT packaging slice lives in
`razero-compiler/AOT_PACKAGING_ABI.md`. That document defines what is treated as
versioned/link-visible/package-stable versus what remains private/internal.

The end-to-end proof currently lives in
`razero-compiler/src/linker.rs::link_hello_host_executable_runs_example_guest`,
which links the guest into a native executable and verifies that it prints:

```text
hello world from guest
```
