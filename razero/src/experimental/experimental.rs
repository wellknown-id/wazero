use crate::api::wasm::{Function, FunctionDefinition, Global, Module};

pub type ProgramCounter = u64;

pub trait InternalModule {
    fn num_global(&self) -> usize;
    fn global(&self, index: usize) -> Global;
}

pub trait InternalFunction {
    fn definition(&self) -> &FunctionDefinition;
    fn source_offset_for_pc(&self, pc: ProgramCounter) -> u64;
}

impl InternalModule for Module {
    fn num_global(&self) -> usize {
        Module::num_global(self)
    }

    fn global(&self, index: usize) -> Global {
        Module::global(self, index)
    }
}

impl InternalFunction for Function {
    fn definition(&self) -> &FunctionDefinition {
        Function::definition(self)
    }

    fn source_offset_for_pc(&self, pc: ProgramCounter) -> u64 {
        self.source_offset_for_pc(pc)
    }
}

#[cfg(test)]
mod tests {
    use super::{InternalFunction, InternalModule};
    use crate::{config::ModuleConfig, runtime::Runtime, Context, ValueType};

    #[test]
    fn internal_module_exposes_non_exported_globals() {
        let runtime = Runtime::new();
        let module = runtime
            .instantiate_binary(
                &[
                    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7f, 0x00,
                    0x41, 0x2a, 0x0b,
                ],
                ModuleConfig::new(),
            )
            .unwrap();

        assert_eq!(1, InternalModule::num_global(&module));
        assert_eq!(
            ValueType::I32,
            InternalModule::global(&module, 0).value_type()
        );
    }

    #[test]
    fn internal_function_delegates_definition_and_source_offset() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(vec![1]), &[], &[ValueType::I32])
            .with_name("one")
            .export("one")
            .instantiate(&Context::default())
            .unwrap();

        let function = module.exported_function("one").unwrap();
        assert_eq!("one", InternalFunction::definition(&function).name());
        assert_eq!(0, InternalFunction::source_offset_for_pc(&function, 123));
    }
}
