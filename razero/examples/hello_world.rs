use razero::{api::wasm::ValueType, Context, ModuleConfig, Runtime, RuntimeError};

const HELLO_WORLD_WASM: &[u8] =
    include_bytes!("../../examples/hello-host/testdata/hello_world.wasm");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new();

    runtime
        .new_host_module_builder("env")
        .new_function_builder()
        .with_func(
            |_ctx, module, params| {
                let offset = usize::try_from(params[0])
                    .map_err(|_| RuntimeError::new("print offset does not fit usize"))?;
                let len = usize::try_from(params[1])
                    .map_err(|_| RuntimeError::new("print length does not fit usize"))?;
                let memory = module
                    .memory()
                    .ok_or_else(|| RuntimeError::new("guest module did not expose memory"))?;
                let bytes = memory.read(offset, len).ok_or_else(|| {
                    RuntimeError::new("print range is out of guest memory bounds")
                })?;
                let message = std::str::from_utf8(&bytes).map_err(|err| {
                    RuntimeError::new(format!("guest string is not utf-8: {err}"))
                })?;
                println!("{message}");
                Ok(Vec::new())
            },
            &[ValueType::I32, ValueType::I32],
            &[],
        )
        .with_name("print")
        .with_parameter_names(&["ptr", "len"])
        .export("print")
        .instantiate(&Context::default())?;

    let module = runtime.instantiate_binary(
        HELLO_WORLD_WASM,
        ModuleConfig::new().with_name("hello-world"),
    )?;

    module
        .exported_function("run")
        .ok_or_else(|| RuntimeError::new("guest module did not export run"))?
        .call(&[])?;

    Ok(())
}
