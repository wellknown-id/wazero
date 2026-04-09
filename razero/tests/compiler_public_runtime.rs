use razero::{Context, ModuleConfig, PrecompiledArtifact, Runtime, RuntimeConfig, ValueType};

const ADD_ONE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f,
    0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x00, 0x0a, 0x09, 0x01,
    0x07, 0x00, 0x20, 0x00, 0x41, 0x01, 0x6a, 0x0b,
];

const HOST_IMPORT_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f,
    0x02, 0x0b, 0x01, 0x03, b'e', b'n', b'v', 0x03, b'i', b'n', b'c', 0x00, 0x00, 0x03, 0x02, 0x01,
    0x00, 0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, 0x0a, 0x08, 0x01, 0x06, 0x00, 0x20,
    0x00, 0x10, 0x00, 0x0b,
];

fn register_increment_host_module(runtime: &Runtime) {
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();
}

#[test]
fn compiler_public_runtime_executes_guest_exports() {
    if !razero_platform::compiler_supported() {
        return;
    }

    let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
    let compiled = runtime.compile(ADD_ONE_WASM).unwrap();
    let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();

    assert_eq!(
        vec![42],
        module
            .exported_function("run")
            .unwrap()
            .call(&[41])
            .unwrap()
    );
}

#[test]
fn compiler_public_runtime_executes_guest_host_imports() {
    if !razero_platform::compiler_supported() {
        return;
    }

    let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let guest = runtime
        .instantiate_binary(HOST_IMPORT_WASM, ModuleConfig::new())
        .unwrap();

    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
}

#[test]
fn compiler_public_runtime_executes_precompiled_artifact_round_trip() {
    if !razero_platform::compiler_supported() {
        return;
    }

    let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
    let artifact = runtime.build_precompiled_artifact(ADD_ONE_WASM).unwrap();
    let encoded = artifact.encode();
    let decoded = PrecompiledArtifact::decode(&encoded).unwrap();
    let module = runtime
        .instantiate_precompiled_artifact(&decoded, ModuleConfig::new())
        .unwrap();

    assert_eq!(
        vec![42],
        module
            .exported_function("run")
            .unwrap()
            .call(&[41])
            .unwrap()
    );
}

#[test]
fn compiler_public_runtime_executes_host_imports_from_precompiled_artifact() {
    if !razero_platform::compiler_supported() {
        return;
    }

    let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, _module, params| Ok(vec![params[0] + 1]),
            &[ValueType::I32],
            &[ValueType::I32],
        )
        .export("inc")
        .instantiate(&Context::default())
        .unwrap();

    let artifact = runtime
        .build_precompiled_artifact(HOST_IMPORT_WASM)
        .unwrap();
    let guest = runtime
        .instantiate_precompiled_artifact(&artifact, ModuleConfig::new())
        .unwrap();

    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
}

#[test]
fn public_runtime_executes_multiple_host_imports_across_supported_flows() {
    for config in [
        RuntimeConfig::new_interpreter(),
        RuntimeConfig::new_compiler(),
    ] {
        if config.engine_kind() == razero::RuntimeEngineKind::Compiler
            && !razero_platform::compiler_supported()
        {
            continue;
        }

        let runtime = Runtime::with_config(config);
        register_increment_host_module(&runtime);
        let guest = runtime
            .instantiate_binary(HOST_IMPORT_WASM, ModuleConfig::new())
            .unwrap();
        assert_eq!(
            vec![42],
            guest.exported_function("run").unwrap().call(&[41]).unwrap()
        );
    }

    if !razero_platform::compiler_supported() {
        return;
    }

    let runtime = Runtime::with_config(RuntimeConfig::new_compiler());
    register_increment_host_module(&runtime);
    let artifact = runtime
        .build_precompiled_artifact(HOST_IMPORT_WASM)
        .unwrap();
    let guest = runtime
        .instantiate_precompiled_artifact(&artifact, ModuleConfig::new())
        .unwrap();

    assert_eq!(
        vec![42],
        guest.exported_function("run").unwrap().call(&[41]).unwrap()
    );
}

#[test]
fn interpreter_runtime_rejects_precompiled_artifacts() {
    if !razero_platform::compiler_supported() {
        return;
    }

    let compiler_runtime = Runtime::with_config(RuntimeConfig::new_compiler());
    let artifact = compiler_runtime
        .build_precompiled_artifact(ADD_ONE_WASM)
        .unwrap();
    let interpreter_runtime = Runtime::with_config(RuntimeConfig::new_interpreter());
    let err = match interpreter_runtime
        .instantiate_precompiled_artifact(&artifact, ModuleConfig::new())
    {
        Ok(_) => panic!("interpreter runtime unexpectedly accepted a precompiled artifact"),
        Err(err) => err,
    };

    assert!(err
        .to_string()
        .contains("precompiled artifacts are unsupported by this engine"));
}
