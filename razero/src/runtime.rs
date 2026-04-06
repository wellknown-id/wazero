use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use crate::{
    api::{
        error::{ExitError, Result, RuntimeError},
        wasm::{
            CustomSection, FunctionDefinition, Global, GlobalValue, HostCallback, Memory,
            MemoryDefinition, Module, RuntimeModuleRegistry, ValueType,
        },
    },
    builder::HostModuleBuilder,
    config::{CompiledModule, CompiledModuleInner, ModuleConfig, RuntimeConfig},
    ctx_keys::Context,
    experimental::memory::{DefaultMemoryAllocator, MemoryAllocator},
};

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    config: RuntimeConfig,
    modules: RuntimeModuleRegistry,
    closed: AtomicBool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::with_config(RuntimeConfig::new())
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: RuntimeConfig) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                config,
                modules: Arc::new(Mutex::new(BTreeMap::new())),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.inner.config
    }

    pub fn compile(&self, bytes: &[u8]) -> Result<CompiledModule> {
        self.fail_if_closed()?;
        if let Some(cache) = self.inner.config.compilation_cache() {
            let key = cache_key(bytes);
            if let Some(cached) = cache.get(&key) {
                return parse_binary_module(&cached, &self.inner.config);
            }
            let compiled = parse_binary_module(bytes, &self.inner.config)?;
            cache.insert(&key, bytes);
            return Ok(compiled);
        }
        parse_binary_module(bytes, &self.inner.config)
    }

    pub fn instantiate(&self, compiled: &CompiledModule, config: ModuleConfig) -> Result<Module> {
        self.instantiate_with_context(&Context::default(), compiled, config)
    }

    pub fn instantiate_with_context(
        &self,
        ctx: &Context,
        compiled: &CompiledModule,
        config: ModuleConfig,
    ) -> Result<Module> {
        self.fail_if_closed()?;
        if compiled.is_closed() {
            return Err(RuntimeError::new("compiled module is closed"));
        }
        let name = if config.name_set() {
            config.name().map(ToOwned::to_owned)
        } else {
            compiled.name().map(ToOwned::to_owned)
        };
        if let Some(name) = &name {
            let modules = self.inner.modules.lock().expect("runtime modules poisoned");
            if modules.contains_key(name) {
                return Err(RuntimeError::new(format!(
                    "module[{name}] has already been instantiated"
                )));
            }
        }

        let memory = instantiate_memory(
            ctx,
            compiled.inner().exported_memories.values().next().cloned(),
            &self.inner.config,
        )?;
        let globals = compiled.inner().exported_globals.clone();
        let module = Module::new(
            name.clone(),
            compiled.inner().exported_functions.clone(),
            compiled.inner().host_callbacks.clone(),
            self.inner.config.fuel(),
            memory,
            globals,
            ctx.close_notifier.clone(),
            Some(Arc::downgrade(&self.inner.modules)),
        );

        if let Some(name) = name {
            self.inner
                .modules
                .lock()
                .expect("runtime modules poisoned")
                .insert(name, module.clone());
        }
        Ok(module)
    }

    pub fn instantiate_binary(&self, bytes: &[u8], config: ModuleConfig) -> Result<Module> {
        let compiled = self.compile(bytes)?;
        self.instantiate(&compiled, config)
    }

    pub fn new_host_module_builder(&self, module_name: impl Into<String>) -> HostModuleBuilder {
        HostModuleBuilder::attached(self.clone(), module_name)
    }

    pub fn module(&self, module_name: &str) -> Option<Module> {
        self.inner
            .modules
            .lock()
            .expect("runtime modules poisoned")
            .get(module_name)
            .cloned()
    }

    pub fn close(&self, ctx: &Context) -> Result<()> {
        self.close_with_exit_code(ctx, 0)
    }

    pub fn close_with_exit_code(&self, ctx: &Context, exit_code: u32) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let modules = self
            .inner
            .modules
            .lock()
            .expect("runtime modules poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for module in modules {
            module.close_with_exit_code(ctx, exit_code)?;
        }
        self.inner
            .modules
            .lock()
            .expect("runtime modules poisoned")
            .clear();
        Ok(())
    }

    fn fail_if_closed(&self) -> Result<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            Err(ExitError::new(0).into())
        } else {
            Ok(())
        }
    }
}

fn instantiate_memory(
    ctx: &Context,
    definition: Option<MemoryDefinition>,
    config: &RuntimeConfig,
) -> Result<Option<Memory>> {
    let Some(definition) = definition else {
        return Ok(None);
    };
    let allocator: Arc<dyn MemoryAllocator> = if let Some(allocator) = ctx.memory_allocator.clone()
    {
        allocator
    } else if config.secure_mode() {
        Arc::new(DefaultMemoryAllocator)
    } else {
        Arc::new(DefaultMemoryAllocator)
    };
    let max_pages = definition
        .maximum_pages()
        .unwrap_or(config.memory_limit_pages());
    let max_bytes = max_pages as usize * 65_536;
    let linear_memory = allocator
        .allocate(definition.minimum_pages() as usize * 65_536, max_bytes)
        .ok_or_else(|| RuntimeError::new("memory allocation failed"))?;
    Ok(Some(Memory::new(definition, linear_memory)))
}

fn cache_key(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}-{:x}", bytes.len())
}

fn parse_binary_module(bytes: &[u8], config: &RuntimeConfig) -> Result<CompiledModule> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
        return Err(RuntimeError::new("invalid magic number"));
    }
    if &bytes[4..8] != [0x01, 0x00, 0x00, 0x00] {
        return Err(RuntimeError::new("invalid version header"));
    }

    let mut reader = Reader::new(&bytes[8..]);
    let mut custom_sections = Vec::new();
    let mut module_name = None;
    let mut types = Vec::<(Vec<ValueType>, Vec<ValueType>)>::new();
    let mut imported_functions = Vec::new();
    let mut defined_type_indices = Vec::new();
    let mut imported_memories = Vec::new();
    let mut defined_memory = None;
    let mut globals = Vec::new();
    let mut exports = Vec::<(String, u8, u32)>::new();
    let mut import_function_count = 0_u32;

    while !reader.is_empty() {
        let section_id = reader.byte()?;
        let section_len = reader.var_u32()? as usize;
        let section_bytes = reader.bytes(section_len)?;
        let mut section = Reader::new(section_bytes);
        match section_id {
            0 => {
                let name = section.name()?;
                let payload = section.remaining().to_vec();
                if config.custom_sections() {
                    custom_sections.push(CustomSection::new(name.clone(), payload.clone()));
                }
                if name == "name" {
                    let mut subsection = Reader::new(&payload);
                    while !subsection.is_empty() {
                        let id = subsection.byte()?;
                        let len = subsection.var_u32()? as usize;
                        let bytes = subsection.bytes(len)?;
                        if id == 0 {
                            module_name = Some(Reader::new(bytes).name()?);
                        }
                    }
                }
            }
            1 => {
                let count = section.var_u32()? as usize;
                for _ in 0..count {
                    if section.byte()? != 0x60 {
                        return Err(RuntimeError::new("unsupported function type"));
                    }
                    let params = read_value_types(&mut section)?;
                    let results = read_value_types(&mut section)?;
                    types.push((params, results));
                }
            }
            2 => {
                let count = section.var_u32()? as usize;
                for _ in 0..count {
                    let import_module = section.name()?;
                    let import_name = section.name()?;
                    match section.byte()? {
                        0x00 => {
                            let type_index = section.var_u32()? as usize;
                            let (params, results) = types
                                .get(type_index)
                                .cloned()
                                .ok_or_else(|| RuntimeError::new("function type out of range"))?;
                            imported_functions.push(
                                FunctionDefinition::new(import_name.clone())
                                    .with_module_name(module_name.clone())
                                    .with_signature(params, results)
                                    .with_import(import_module, import_name),
                            );
                            import_function_count += 1;
                        }
                        0x02 => {
                            imported_memories.push(parse_memory_definition(
                                &mut section,
                                module_name.clone(),
                                Some((import_module, import_name)),
                                config,
                            )?);
                        }
                        0x03 => {
                            globals.push(parse_global(&mut section)?);
                        }
                        _ => skip_import_desc(&mut section)?,
                    }
                }
            }
            3 => {
                let count = section.var_u32()? as usize;
                for _ in 0..count {
                    defined_type_indices.push(section.var_u32()?);
                }
            }
            5 => {
                let count = section.var_u32()? as usize;
                if count > 0 {
                    defined_memory = Some(parse_memory_definition(
                        &mut section,
                        module_name.clone(),
                        None,
                        config,
                    )?);
                }
            }
            6 => {
                let count = section.var_u32()? as usize;
                for _ in 0..count {
                    globals.push(parse_global(&mut section)?);
                }
            }
            7 => {
                let count = section.var_u32()? as usize;
                for _ in 0..count {
                    exports.push((section.name()?, section.byte()?, section.var_u32()?));
                }
            }
            _ => {}
        }
    }

    let mut exported_functions = BTreeMap::new();
    let mut exported_memories = BTreeMap::new();
    let mut exported_globals = BTreeMap::new();
    for (name, kind, index) in exports {
        match kind {
            0x00 => {
                let definition = if index < import_function_count {
                    imported_functions
                        .get(index as usize)
                        .cloned()
                        .unwrap_or_else(|| FunctionDefinition::new(name.clone()))
                        .with_export_name(name.clone())
                } else {
                    let type_index = defined_type_indices
                        .get((index - import_function_count) as usize)
                        .copied()
                        .ok_or_else(|| RuntimeError::new("exported function index out of range"))?
                        as usize;
                    let (params, results) = types
                        .get(type_index)
                        .cloned()
                        .ok_or_else(|| RuntimeError::new("exported function type out of range"))?;
                    FunctionDefinition::new(name.clone())
                        .with_module_name(module_name.clone())
                        .with_signature(params, results)
                        .with_export_name(name.clone())
                };
                exported_functions.insert(name, definition);
            }
            0x02 => {
                let definition = defined_memory
                    .clone()
                    .unwrap_or_else(|| {
                        MemoryDefinition::new(0, None).with_module_name(module_name.clone())
                    })
                    .with_export_name(name.clone());
                exported_memories.insert(name, definition);
            }
            0x03 => {
                if let Some(global) = globals.get(index as usize) {
                    exported_globals.insert(name, global.clone());
                }
            }
            _ => {}
        }
    }

    Ok(CompiledModule::new(CompiledModuleInner {
        name: module_name,
        bytes: bytes.to_vec(),
        imported_functions,
        exported_functions,
        imported_memories,
        exported_memories,
        exported_globals,
        custom_sections,
        host_callbacks: BTreeMap::<String, HostCallback>::new(),
        closed: std::sync::atomic::AtomicBool::new(false),
    }))
}

fn parse_memory_definition(
    reader: &mut Reader<'_>,
    module_name: Option<String>,
    import: Option<(String, String)>,
    config: &RuntimeConfig,
) -> Result<MemoryDefinition> {
    let flags = reader.byte()?;
    let minimum_pages = reader.var_u32()?;
    let maximum_pages = if flags & 0x01 != 0 {
        Some(reader.var_u32()?.min(config.memory_limit_pages()))
    } else {
        Some(config.memory_limit_pages())
    };
    if minimum_pages > config.memory_limit_pages() {
        return Err(RuntimeError::new(format!(
            "section memory: min {minimum_pages} pages over limit of {} pages",
            config.memory_limit_pages()
        )));
    }
    let mut definition =
        MemoryDefinition::new(minimum_pages, maximum_pages).with_module_name(module_name);
    if let Some((import_module, import_name)) = import {
        definition = definition.with_import(import_module, import_name);
    }
    Ok(definition)
}

fn parse_global(reader: &mut Reader<'_>) -> Result<Global> {
    let value_type = match reader.byte()? {
        0x7f => ValueType::I32,
        0x7e => ValueType::I64,
        0x7d => ValueType::F32,
        0x7c => ValueType::F64,
        _ => return Err(RuntimeError::new("unsupported global type")),
    };
    let mutable = reader.byte()? != 0;
    let opcode = reader.byte()?;
    let value = match (value_type, opcode) {
        (ValueType::I32, 0x41) => GlobalValue::I32(reader.var_i32()?),
        (ValueType::I64, 0x42) => GlobalValue::I64(reader.var_i64()?),
        (ValueType::F32, 0x43) => GlobalValue::F32(u32::from_le_bytes(
            reader.bytes(4)?.try_into().expect("len"),
        )),
        (ValueType::F64, 0x44) => GlobalValue::F64(u64::from_le_bytes(
            reader.bytes(8)?.try_into().expect("len"),
        )),
        _ => return Err(RuntimeError::new("unsupported global initializer")),
    };
    let _end = reader.byte()?;
    Ok(Global::new(value, mutable))
}

fn skip_import_desc(reader: &mut Reader<'_>) -> Result<()> {
    match reader.byte()? {
        _ => Ok(()),
    }
}

fn read_value_types(reader: &mut Reader<'_>) -> Result<Vec<ValueType>> {
    let count = reader.var_u32()? as usize;
    let mut value_types = Vec::with_capacity(count);
    for _ in 0..count {
        value_types.push(match reader.byte()? {
            0x7f => ValueType::I32,
            0x7e => ValueType::I64,
            0x7d => ValueType::F32,
            0x7c => ValueType::F64,
            0x7b => ValueType::V128,
            0x6f => ValueType::ExternRef,
            0x70 => ValueType::FuncRef,
            _ => return Err(RuntimeError::new("unsupported value type")),
        });
    }
    Ok(value_types)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn is_empty(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }

    fn byte(&mut self) -> Result<u8> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or_else(|| RuntimeError::new("unexpected end of input"))?;
        self.position += 1;
        Ok(byte)
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| RuntimeError::new("unexpected end of input"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| RuntimeError::new("unexpected end of input"))?;
        self.position = end;
        Ok(bytes)
    }

    fn name(&mut self) -> Result<String> {
        let len = self.var_u32()? as usize;
        let bytes = self.bytes(len)?;
        let value =
            std::str::from_utf8(bytes).map_err(|_| RuntimeError::new("name is not valid UTF-8"))?;
        Ok(value.to_string())
    }

    fn var_u32(&mut self) -> Result<u32> {
        let mut result = 0_u32;
        let mut shift = 0_u32;
        loop {
            let byte = self.byte()?;
            result |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    fn var_i32(&mut self) -> Result<i32> {
        Ok(self.var_u32()? as i32)
    }

    fn var_i64(&mut self) -> Result<i64> {
        let mut result = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = self.byte()?;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result as i64);
            }
            shift += 7;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicI64, AtomicU32, Ordering},
        Arc, Mutex,
    };

    use super::Runtime;
    use crate::{
        api::{
            error::RuntimeError,
            wasm::{FunctionDefinition, ValueType},
        },
        config::ModuleConfig,
        ctx_keys::Context,
        experimental::{
            add_fuel, get_snapshotter, get_yielder, remaining_fuel, with_close_notifier,
            with_fuel_controller, with_function_listener_factory, with_snapshotter, with_yielder,
            CloseNotifier, FuelController, FunctionListener, FunctionListenerFactory,
        },
    };

    #[derive(Clone)]
    struct TestFuelController {
        budget: i64,
        consumed: Arc<AtomicI64>,
    }

    impl FuelController for TestFuelController {
        fn budget(&self) -> i64 {
            self.budget
        }

        fn consumed(&self, amount: i64) {
            self.consumed.fetch_add(amount, Ordering::SeqCst);
        }
    }

    struct RecordingListener {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FunctionListener for RecordingListener {
        fn before(
            &self,
            _ctx: &Context,
            _module: &crate::Module,
            definition: &FunctionDefinition,
            params: &[u64],
        ) {
            self.events
                .lock()
                .expect("listener events poisoned")
                .push(format!("before:{}:{params:?}", definition.name()));
        }

        fn after(
            &self,
            _ctx: &Context,
            _module: &crate::Module,
            definition: &FunctionDefinition,
            results: &[u64],
        ) {
            self.events
                .lock()
                .expect("listener events poisoned")
                .push(format!("after:{}:{results:?}", definition.name()));
        }
    }

    struct RecordingListenerFactory {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FunctionListenerFactory for RecordingListenerFactory {
        fn new_listener(
            &self,
            _definition: &FunctionDefinition,
        ) -> Option<Arc<dyn FunctionListener>> {
            Some(Arc::new(RecordingListener {
                events: self.events.clone(),
            }))
        }
    }

    struct RecordingCloseNotifier {
        exit_code: Arc<AtomicU32>,
    }

    impl CloseNotifier for RecordingCloseNotifier {
        fn close_notify(&self, _ctx: &Context, exit_code: u32) {
            self.exit_code.store(exit_code, Ordering::SeqCst);
        }
    }

    #[test]
    fn runtime_rejects_invalid_magic() {
        let runtime = Runtime::new();
        let err = match runtime.compile(b"not-wasm") {
            Ok(_) => panic!("expected invalid magic error"),
            Err(err) => err,
        };
        assert_eq!("invalid magic number", err.to_string());
    }

    #[test]
    fn runtime_can_instantiate_empty_module() {
        let runtime = Runtime::new();
        let bytes = b"\0asm\x01\0\0\0";
        let compiled = runtime.compile(bytes).unwrap();
        let module = runtime
            .instantiate_with_context(&Context::default(), &compiled, ModuleConfig::new())
            .unwrap();
        assert!(!module.is_closed());
    }

    #[test]
    fn host_module_calls_listeners_and_tracks_fuel() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_func(
                |ctx, _module, params| {
                    assert_eq!(4, remaining_fuel(&ctx).unwrap());
                    add_fuel(&ctx, -2).unwrap();
                    Ok(vec![params[0] + params[1]])
                },
                &[ValueType::I32, ValueType::I32],
                &[ValueType::I32],
            )
            .with_name("add")
            .export("add")
            .instantiate(&Context::default())
            .unwrap();

        let consumed = Arc::new(AtomicI64::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let call_ctx = with_fuel_controller(
            &with_function_listener_factory(
                &Context::default(),
                RecordingListenerFactory {
                    events: events.clone(),
                },
            ),
            TestFuelController {
                budget: 5,
                consumed: consumed.clone(),
            },
        );

        let results = module
            .exported_function("add")
            .unwrap()
            .call_with_context(&call_ctx, &[20, 22])
            .unwrap();

        assert_eq!(vec![42], results);
        assert_eq!(3, consumed.load(Ordering::SeqCst));
        assert_eq!(
            vec!["before:add:[20, 22]", "after:add:[42]"],
            *events.lock().expect("events poisoned")
        );
    }

    #[test]
    fn memory_supports_read_write_and_grow() {
        let runtime = Runtime::new();
        let bytes = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07,
            0x0a, 0x01, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00,
        ];
        let compiled = runtime.compile(&bytes).unwrap();
        let module = runtime.instantiate(&compiled, ModuleConfig::new()).unwrap();
        let memory = module.exported_memory("memory").unwrap();

        assert_eq!(65_536, memory.size());
        assert!(memory.write_u32_le(8, 0x1122_3344));
        assert_eq!(Some(0x1122_3344), memory.read_u32_le(8));
        assert_eq!(Some(1), memory.grow(1));
        assert_eq!(131_072, memory.size());
    }

    #[test]
    fn compiled_module_close_prevents_instantiation() {
        let runtime = Runtime::new();
        let compiled = runtime.compile(b"\0asm\x01\0\0\0").unwrap();
        compiled.close();
        let err = match runtime.instantiate(&compiled, ModuleConfig::new()) {
            Ok(_) => panic!("expected closed compiled module error"),
            Err(err) => err,
        };
        assert_eq!("compiled module is closed", err.to_string());
    }

    #[test]
    fn host_stack_callback_still_supported() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_stack_callback(
                |_ctx, _module, stack| {
                    stack[0] += stack[1];
                    Ok(())
                },
                &[ValueType::I32, ValueType::I32],
                &[ValueType::I32],
            )
            .export("add")
            .instantiate(&Context::default())
            .unwrap();

        assert_eq!(
            vec![42],
            module.exported_function("add").unwrap().call(&[20, 22]).unwrap()
        );
    }

    #[test]
    fn close_notifier_runs_on_module_close() {
        let runtime = Runtime::new();
        let exit_code = Arc::new(AtomicU32::new(0));
        let instantiate_ctx = with_close_notifier(
            &Context::default(),
            RecordingCloseNotifier {
                exit_code: exit_code.clone(),
            },
        );

        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(|_ctx, _module, _params| Ok(Vec::new()), &[], &[])
            .export("noop")
            .instantiate(&instantiate_ctx)
            .unwrap();

        module.close_with_exit_code(&instantiate_ctx, 7).unwrap();
        assert_eq!(7, exit_code.load(Ordering::SeqCst));
    }

    #[test]
    fn snapshot_and_yield_hooks_work_for_host_functions() {
        let runtime = Runtime::new();
        let module = runtime
            .new_host_module_builder("env")
            .new_function_builder()
            .with_callback(
                |ctx, _module, params| {
                    if params[0] == 0 {
                        let snapshotter = get_snapshotter(&ctx).unwrap();
                        snapshotter.snapshot().restore(&[99]);
                        return Ok(vec![1]);
                    }
                    get_yielder(&ctx).unwrap().r#yield();
                    Ok(vec![0])
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("hook")
            .instantiate(&Context::default())
            .unwrap();

        let snapshot_ctx = with_snapshotter(&Context::default());
        let results = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&snapshot_ctx, &[0])
            .unwrap();
        assert_eq!(vec![99], results);

        let yield_ctx = with_yielder(&Context::default());
        let err = module
            .exported_function("hook")
            .unwrap()
            .call_with_context(&yield_ctx, &[1])
            .unwrap_err();
        let RuntimeError::Yield(yield_error) = err else {
            panic!("expected yield error");
        };
        let resumed = yield_error
            .resumer()
            .resume(&Context::default(), &[55])
            .unwrap();
        assert_eq!(vec![55], resumed);
    }
}
