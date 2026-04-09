# Razero Rust AOT packaging ABI

This document freezes the currently supported Rust AOT packaging contract for the
Linux/ELF-first native-link path. It defines the surface that packaged artifacts,
linkers, and runtime-support code may rely on today.

## Scope and ownership

- `razero` owns runtime embedding APIs, interpreter availability, and the separate
  precompiled-artifact format used by `PrecompiledArtifact`.
- `razero-compiler` owns AOT code generation, relocatable object emission,
  metadata sidecars, `.razero-package` bundles, and linker/runtime-support ABI.
- `razero-ffi` remains optional. Packaged native executables must not require it.
- WASI stays out of the core crates. Host behavior must be supplied explicitly by
  the embedder or linker inputs.
- Linux/ELF is the first shipping packaging target, with x86_64 and AArch64 the
  currently implemented architectures. The general native-link surface is C ABI
  first.
- Interpreter mode remains required product surface. Packaged native executables
  are an additional deployment target, not a replacement runtime mode.

## Stable artifacts

### 1. AOT metadata sidecar

The serialized metadata sidecar produced by
`razero_compiler::aot::serialize_aot_metadata(...)` is a supported ABI surface.

- Magic header: `RAZEROAT` (`AOT_METADATA_MAGIC`)
- Target tuple: architecture + operating system bytes
- Module identity: 32-byte module id
- Module/function/type/import/export metadata
- Relocations
- Module-context layout metadata
- Execution-context layout metadata
- Helper descriptors
- Data/table/global/memory metadata
- Global initializer and element-segment metadata
- Source map and shape flags

Compatibility rules:

- Changing serialized field meaning is an ABI break.
- Append-only growth is preferred whenever possible.
- If the sidecar schema becomes incompatible, the format header must change and
  matching linker/runtime support must be updated together.
- `AotExecutionContextMetadata::abi_version` must change whenever the execution
  context layout changes incompatibly.

### 2. `.razero-package` bundle

`link_native_executable(...)` writes a `.razero-package` file beside the linked
native executable. The bundle contains metadata only; it does **not** embed the
ELF object bytes or static libraries passed to the linker.

Binary layout:

1. 8-byte magic `RZPKG001` (`NATIVE_PACKAGE_MAGIC`)
2. little-endian `u32` module count
3. repeated per module:
   - little-endian `u32` UTF-8 module-name length
   - module-name bytes
   - little-endian `u64` metadata-sidecar length
   - serialized metadata sidecar bytes
4. little-endian `u32` packaged host-import descriptor count
5. repeated per descriptor:
   - little-endian `u32` guest-module-name length
   - guest-module-name bytes
   - little-endian `u32` import-module length
   - import-module bytes
   - little-endian `u32` import-name length
   - import-name bytes
   - little-endian `u32` function-import index
   - little-endian `u32` type index
   - little-endian `u32` host-symbol-name length
   - host-symbol-name bytes

The bundle is represented in Rust by:

- `NativePackageMetadataEntry`
- `PackagedHostImportDescriptor`
- `NativePackageMetadataBundle`
- `serialize_native_package_metadata_bundle(...)`
- `deserialize_native_package_metadata_bundle(...)`

### 3. Execution-context ABI

`AotExecutionContextMetadata` is a frozen part of the packaging contract.
Packaged/linkable code may rely on:

- `abi_version`
- `size`
- every named byte offset field in the struct

These offsets describe the layout of the compiler/runtime execution context used
by linked native code. Incompatible layout changes require an execution-context
ABI version bump.

### 4. Module-context ABI

`AotModuleContextMetadata` is also part of the supported packaging ABI.

- `total_size` defines the backing storage size required for the module context.
- Every named field is a byte offset from the module-context base pointer.
- A negative offset means the current module shape does not supply that region.

This layout is especially relevant for generated startup/wrapper code such as the
current `hello-host` example and any future generalized runtime-state packaging.

### 5. Helper IDs

`AotHelperId` numeric assignments are stable serialized ABI:

1. `MemoryGrow`
2. `StackGrow`
3. `CheckModuleExitCode`
4. `TableGrow`
5. `RefFunc`
6. `Memmove`
7. `MemoryWait32`
8. `MemoryWait64`
9. `MemoryNotify`

`AotHelperMetadata` binds each helper id to an execution-context offset and, when
applicable, an exit code. New helper ids must be append-only.

## Stable link-visible symbols

### Raw linked Wasm function symbols

ELF objects emitted by `emit_relocatable_object()` expose link-visible raw Wasm
entry symbols with this exact shape:

`razero_wasm_function_<module-id-hex>_<wasm-function-index>`

These are the raw machine-code entrypoints consumed by native-link packaging.

### Generated C ABI wrapper symbols

`link_native_executable(...)` generates stable C ABI wrapper symbols with this
shape:

`razero_cabi_<sanitized-module-name>_function_<wasm-function-index>`

The sanitized module name replaces non-identifier bytes with `_` and prefixes an
initial digit with `_`.

### Generated preamble symbols

For each supported exported function type, the linker generates:

`razero_cabi_<sanitized-module-name>_type_<type-index>_preamble`

### Packaged host-import trampoline symbols

Current packaged host-import flows resolve guest object references through
symbols with this shape:

`razero_import_function_<function-import-index>_<sanitized-import-module>_<sanitized-import-name>`

The `.razero-package` bundle records the matching `PackagedHostImportDescriptor`
entries, including the embedder-owned host symbol each import dispatches to.

### Linux native support entrypoints

The generated wrappers rely on these support symbols:

- `razero_amd64_entrypoint`
- `razero_amd64_after_go_function_call_entrypoint`
- `razero_arm64_entrypoint`
- `razero_arm64_after_go_function_call_entrypoint`

These are part of the Linux native packaging contract for the current native-link
path.

## Explicitly private or narrow details

The following are **not** part of the frozen generic packaging ABI:

- temporary `.razero-link` work directories
- generated C or assembly file names inside that work directory
- compilation-scope-only object symbols such as `razero_wasm_function_<index>`
- any future generalized host-import or runtime-state packaging surface that is
  not yet documented here

The `hello-host` path remains intentionally narrow in its host-import support:
one `env.print(i32, i32)` import with example-specific behavior. Generic
runtime-state packaging should instead rely on metadata-driven initialization
from the serialized AOT sidecar.

## Current supported surface vs. future expansion

Supported today:

- Linux/x86_64 ELF relocatable objects
- Linux/aarch64 ELF relocatable objects
- metadata sidecars
- `.razero-package` metadata bundles
- linked raw Wasm entry symbols
- scalar C ABI-compatible exported wrappers
- metadata-driven linked runtime-state packaging for no-import modules with
  local memory/globals/tables, active data/element initialization, and start
  sections
- packaged host-import descriptors plus explicit host-symbol dispatch
- `hello-host` packaging built on that reusable host-import layer

Not yet generalized:

- imported memory/global/table packaging
- multi-module runtime-state packaging
- WASI integration in core crates

Future expansion must preserve the product boundaries and either remain
backward-compatible with this contract or version incompatible changes
explicitly.
