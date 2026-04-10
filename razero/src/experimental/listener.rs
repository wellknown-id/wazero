use std::sync::Arc;

use crate::{
    api::{
        error::RuntimeError,
        wasm::{FunctionDefinition, Module},
    },
    ctx_keys::Context,
    experimental::{InternalFunction, ProgramCounter},
};

pub trait FunctionListenerFactory: Send + Sync {
    fn new_listener(&self, definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>>;
}

impl<F> FunctionListenerFactory for F
where
    F: Fn(&FunctionDefinition) -> Option<Arc<dyn FunctionListener>> + Send + Sync,
{
    fn new_listener(&self, definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>> {
        (self)(definition)
    }
}

pub trait StackIterator {
    fn next(&mut self) -> bool;
    fn function(&self) -> &dyn InternalFunction;
    fn program_counter(&self) -> ProgramCounter;
}

pub trait FunctionListener: Send + Sync {
    fn before(
        &self,
        _ctx: &Context,
        _module: &Module,
        _definition: &FunctionDefinition,
        _params: &[u64],
        _stack_iterator: &mut dyn StackIterator,
    ) {
    }

    fn after(
        &self,
        _ctx: &Context,
        _module: &Module,
        _definition: &FunctionDefinition,
        _results: &[u64],
    ) {
    }

    fn abort(
        &self,
        _ctx: &Context,
        _module: &Module,
        _definition: &FunctionDefinition,
        _error: &RuntimeError,
    ) {
    }
}

impl<F> FunctionListener for F
where
    F: for<'a> Fn(&Context, &Module, &FunctionDefinition, &[u64], &'a mut dyn StackIterator)
        + Send
        + Sync,
{
    fn before(
        &self,
        ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        params: &[u64],
        stack_iterator: &mut dyn StackIterator,
    ) {
        (self)(ctx, module, definition, params, stack_iterator);
    }
}

pub struct FunctionListenerFn<F>(F);

impl<F> FunctionListenerFn<F> {
    pub fn new(listener: F) -> Self {
        Self(listener)
    }
}

impl<F> FunctionListener for FunctionListenerFn<F>
where
    F: for<'a> Fn(&Context, &Module, &FunctionDefinition, &[u64], &'a mut dyn StackIterator)
        + Send
        + Sync,
{
    fn before(
        &self,
        ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        params: &[u64],
        stack_iterator: &mut dyn StackIterator,
    ) {
        (self.0)(ctx, module, definition, params, stack_iterator);
    }
}

pub struct FunctionListenerFactoryFn<F>(F);

impl<F> FunctionListenerFactoryFn<F> {
    pub fn new(factory: F) -> Self {
        Self(factory)
    }
}

impl<F> FunctionListenerFactory for FunctionListenerFactoryFn<F>
where
    F: Fn(&FunctionDefinition) -> Option<Arc<dyn FunctionListener>> + Send + Sync,
{
    fn new_listener(&self, definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>> {
        (self.0)(definition)
    }
}

pub struct MultiFunctionListenerFactory {
    factories: Vec<Arc<dyn FunctionListenerFactory>>,
}

impl MultiFunctionListenerFactory {
    pub fn new(factories: Vec<Arc<dyn FunctionListenerFactory>>) -> Self {
        Self { factories }
    }
}

impl FunctionListenerFactory for MultiFunctionListenerFactory {
    fn new_listener(&self, definition: &FunctionDefinition) -> Option<Arc<dyn FunctionListener>> {
        let listeners: Vec<_> = self
            .factories
            .iter()
            .filter_map(|factory| factory.new_listener(definition))
            .collect();
        match listeners.len() {
            0 => None,
            1 => listeners.into_iter().next(),
            _ => Some(Arc::new(MultiFunctionListener { listeners })),
        }
    }
}

struct MultiFunctionListener {
    listeners: Vec<Arc<dyn FunctionListener>>,
}

impl FunctionListener for MultiFunctionListener {
    fn before(
        &self,
        ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        params: &[u64],
        stack_iterator: &mut dyn StackIterator,
    ) {
        let frames = capture_stack_frames(stack_iterator);
        for listener in &self.listeners {
            let mut iterator = FrameStackIterator::from_frames(frames.clone());
            listener.before(ctx, module, definition, params, &mut iterator);
        }
    }

    fn after(
        &self,
        ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        results: &[u64],
    ) {
        for listener in &self.listeners {
            listener.after(ctx, module, definition, results);
        }
    }

    fn abort(
        &self,
        ctx: &Context,
        module: &Module,
        definition: &FunctionDefinition,
        error: &RuntimeError,
    ) {
        for listener in &self.listeners {
            listener.abort(ctx, module, definition, error);
        }
    }
}

pub fn with_function_listener_factory(
    ctx: &Context,
    factory: impl FunctionListenerFactory + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.function_listener_factory = Some(Arc::new(factory));
    cloned
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackFrame {
    definition: FunctionDefinition,
    params: Vec<u64>,
    results: Vec<u64>,
    program_counter: ProgramCounter,
    source_offset: u64,
}

impl StackFrame {
    pub fn new(
        definition: FunctionDefinition,
        params: Vec<u64>,
        results: Vec<u64>,
        program_counter: ProgramCounter,
        source_offset: u64,
    ) -> Self {
        Self {
            definition,
            params,
            results,
            program_counter,
            source_offset,
        }
    }

    pub fn definition(&self) -> &FunctionDefinition {
        &self.definition
    }

    pub fn params(&self) -> &[u64] {
        &self.params
    }

    pub fn results(&self) -> &[u64] {
        &self.results
    }

    pub fn program_counter(&self) -> ProgramCounter {
        self.program_counter
    }

    pub fn source_offset(&self) -> u64 {
        self.source_offset
    }
}

#[derive(Clone)]
struct CapturedFrame {
    definition: FunctionDefinition,
    program_counter: ProgramCounter,
    source_offset: u64,
}

#[derive(Clone)]
struct FrameInternalFunction {
    definition: FunctionDefinition,
    source_offset: u64,
}

impl InternalFunction for FrameInternalFunction {
    fn definition(&self) -> &FunctionDefinition {
        &self.definition
    }

    fn source_offset_for_pc(&self, _pc: ProgramCounter) -> u64 {
        self.source_offset
    }
}

pub struct FrameStackIterator {
    frames: Vec<CapturedFrame>,
    functions: Vec<FrameInternalFunction>,
    index: usize,
}

impl FrameStackIterator {
    fn from_frames(frames: Vec<CapturedFrame>) -> Self {
        let functions = frames
            .iter()
            .map(|frame| FrameInternalFunction {
                definition: frame.definition.clone(),
                source_offset: frame.source_offset,
            })
            .collect();
        Self {
            frames,
            functions,
            index: 0,
        }
    }
}

impl StackIterator for FrameStackIterator {
    fn next(&mut self) -> bool {
        self.index += 1;
        self.index <= self.frames.len()
    }

    fn function(&self) -> &dyn InternalFunction {
        &self.functions[self.index - 1]
    }

    fn program_counter(&self) -> ProgramCounter {
        self.frames[self.index - 1].program_counter
    }
}

pub fn new_stack_iterator(stack: &[StackFrame]) -> FrameStackIterator {
    let frames = stack
        .iter()
        .rev()
        .map(|frame| CapturedFrame {
            definition: frame.definition.clone(),
            program_counter: frame.program_counter,
            source_offset: frame.source_offset,
        })
        .collect();
    FrameStackIterator::from_frames(frames)
}

pub fn benchmark_function_listener(
    iterations: usize,
    module: &Module,
    stack: &[StackFrame],
    listener: &dyn FunctionListener,
) {
    assert!(
        !stack.is_empty(),
        "cannot benchmark function listener with an empty stack"
    );
    let ctx = Context::default();
    let definition = stack[0].definition.clone();
    let params = stack[0].params.clone();
    let results = stack[0].results.clone();
    for _ in 0..iterations {
        let mut iterator = new_stack_iterator(stack);
        listener.before(&ctx, module, &definition, &params, &mut iterator);
        listener.after(&ctx, module, &definition, &results);
    }
}

fn capture_stack_frames(stack_iterator: &mut dyn StackIterator) -> Vec<CapturedFrame> {
    let mut frames = Vec::new();
    while stack_iterator.next() {
        let pc = stack_iterator.program_counter();
        let function = stack_iterator.function();
        frames.push(CapturedFrame {
            definition: function.definition().clone(),
            program_counter: pc,
            source_offset: function.source_offset_for_pc(pc),
        });
    }
    frames
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        benchmark_function_listener, new_stack_iterator, FunctionListener, FunctionListenerFactory,
        FunctionListenerFactoryFn, FunctionListenerFn, MultiFunctionListenerFactory, StackFrame,
        StackIterator,
    };
    use crate::{api::wasm::FunctionDefinition, Context, Module, RuntimeError};

    struct RecordingFactory {
        calls: Arc<Mutex<Vec<String>>>,
        label: &'static str,
    }

    impl FunctionListenerFactory for RecordingFactory {
        fn new_listener(
            &self,
            _definition: &FunctionDefinition,
        ) -> Option<Arc<dyn FunctionListener>> {
            Some(Arc::new(RecordingListener {
                calls: self.calls.clone(),
                label: self.label,
            }))
        }
    }

    struct RecordingListener {
        calls: Arc<Mutex<Vec<String>>>,
        label: &'static str,
    }

    impl FunctionListener for RecordingListener {
        fn before(
            &self,
            _ctx: &Context,
            _module: &Module,
            definition: &FunctionDefinition,
            _params: &[u64],
            stack_iterator: &mut dyn StackIterator,
        ) {
            let mut stack = Vec::new();
            while stack_iterator.next() {
                stack.push(format!(
                    "{}@{}:{}",
                    stack_iterator.function().definition().name(),
                    stack_iterator.program_counter(),
                    stack_iterator
                        .function()
                        .source_offset_for_pc(stack_iterator.program_counter())
                ));
            }
            self.calls.lock().expect("calls poisoned").push(format!(
                "{}:{}:{}",
                self.label,
                definition.name(),
                stack.join("|")
            ));
        }
    }

    #[test]
    fn function_listener_fn_only_runs_before() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = FunctionListenerFn::new({
            let calls = calls.clone();
            move |_ctx: &Context,
                  _module: &Module,
                  def: &FunctionDefinition,
                  params: &[u64],
                  stack_iterator: &mut dyn StackIterator| {
                let mut stack = Vec::new();
                while stack_iterator.next() {
                    stack.push(format!(
                        "{}@{}:{}",
                        stack_iterator.function().definition().name(),
                        stack_iterator.program_counter(),
                        stack_iterator
                            .function()
                            .source_offset_for_pc(stack_iterator.program_counter())
                    ));
                }
                calls.lock().expect("calls poisoned").push(format!(
                    "before:{}:{params:?}:{}",
                    def.name(),
                    stack.join("|")
                ));
            }
        });
        let ctx = Context::default();
        let module = Module::new(
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            false,
            0,
            None,
            std::collections::BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            None,
        );
        let def = FunctionDefinition::new("run");
        let mut iterator =
            new_stack_iterator(&[StackFrame::new(def.clone(), vec![1, 2], vec![3], 7, 11)]);
        listener.before(&ctx, &module, &def, &[1, 2], &mut iterator);
        listener.after(&ctx, &module, &def, &[3]);
        listener.abort(&ctx, &module, &def, &RuntimeError::new("boom"));
        assert_eq!(
            vec!["before:run:[1, 2]:run@7:11"],
            *calls.lock().expect("calls poisoned")
        );
    }

    #[test]
    fn function_listener_factory_fn_creates_listener() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let factory = FunctionListenerFactoryFn::new({
            let calls = calls.clone();
            move |definition: &FunctionDefinition| {
                Some(Arc::new(RecordingListener {
                    calls: calls.clone(),
                    label: if definition.name() == "run" {
                        "factory"
                    } else {
                        "unexpected"
                    },
                }) as Arc<dyn FunctionListener>)
            }
        });
        let listener = factory
            .new_listener(&FunctionDefinition::new("run"))
            .expect("listener should be present");
        let module = Module::new(
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            false,
            0,
            None,
            std::collections::BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            None,
        );
        let def = FunctionDefinition::new("run");
        let mut iterator =
            new_stack_iterator(&[StackFrame::new(def.clone(), vec![4], vec![5], 6, 7)]);
        listener.before(&Context::default(), &module, &def, &[4], &mut iterator);

        assert_eq!(
            vec!["factory:run:run@6:7"],
            *calls.lock().expect("calls poisoned")
        );
    }

    #[test]
    fn multi_factory_combines_listeners() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let factories: Vec<Arc<dyn FunctionListenerFactory>> = vec![
            Arc::new(RecordingFactory {
                calls: calls.clone(),
                label: "first",
            }),
            Arc::new(RecordingFactory {
                calls: calls.clone(),
                label: "second",
            }),
        ];

        let composite = MultiFunctionListenerFactory::new(factories)
            .new_listener(&FunctionDefinition::new("run"))
            .expect("listener should be present");
        let module = Module::new(
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            false,
            0,
            None,
            std::collections::BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            None,
        );
        let mut iterator = new_stack_iterator(&[
            StackFrame::new(FunctionDefinition::new("caller"), vec![], vec![], 1, 2),
            StackFrame::new(FunctionDefinition::new("run"), vec![], vec![], 3, 5),
        ]);
        composite.before(
            &Context::default(),
            &module,
            &FunctionDefinition::new("run"),
            &[],
            &mut iterator,
        );

        assert_eq!(
            vec![
                "first:run:run@3:5|caller@1:2",
                "second:run:run@3:5|caller@1:2",
            ],
            *calls.lock().expect("calls poisoned")
        );
    }

    #[test]
    fn benchmark_listener_reuses_stack_shape() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = RecordingListener {
            calls: calls.clone(),
            label: "bench",
        };
        let module = Module::new(
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            false,
            0,
            None,
            std::collections::BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            None,
        );
        let stack = [StackFrame::new(
            FunctionDefinition::new("run"),
            vec![1],
            vec![2],
            9,
            13,
        )];
        benchmark_function_listener(2, &module, &stack, &listener);
        assert_eq!(
            vec!["bench:run:run@9:13", "bench:run:run@9:13"],
            *calls.lock().expect("calls poisoned")
        );
    }

    #[test]
    fn with_function_listener_factory_accepts_closure_factory() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let expected_calls = calls.clone();
        let factory = move |_definition: &FunctionDefinition| {
            Some(Arc::new(RecordingListener {
                calls: calls.clone(),
                label: "closure-factory",
            }) as Arc<dyn FunctionListener>)
        };

        let listener = factory
            .new_listener(&FunctionDefinition::new("run"))
            .expect("listener should be present");
        let module = Module::new(
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            false,
            0,
            None,
            std::collections::BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            None,
        );
        let def = FunctionDefinition::new("run");
        let mut iterator =
            new_stack_iterator(&[StackFrame::new(def.clone(), vec![4], vec![5], 6, 7)]);
        listener.before(&Context::default(), &module, &def, &[4], &mut iterator);

        assert_eq!(
            vec!["closure-factory:run:run@6:7"],
            *expected_calls.lock().expect("calls poisoned")
        );
    }

    #[test]
    fn closure_can_implement_function_listener() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let listener = {
            let calls = calls.clone();
            move |_ctx: &Context,
                  _module: &Module,
                  def: &FunctionDefinition,
                  _params: &[u64],
                  stack_iterator: &mut dyn StackIterator| {
                let mut stack = Vec::new();
                while stack_iterator.next() {
                    stack.push(format!(
                        "{}@{}:{}",
                        stack_iterator.function().definition().name(),
                        stack_iterator.program_counter(),
                        stack_iterator
                            .function()
                            .source_offset_for_pc(stack_iterator.program_counter())
                    ));
                }
                calls.lock().expect("calls poisoned").push(format!(
                    "closure:{}:{}",
                    def.name(),
                    stack.join("|")
                ));
            }
        };
        let ctx = Context::default();
        let module = Module::new(
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            false,
            0,
            None,
            std::collections::BTreeMap::new(),
            Vec::new(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            None,
        );
        let def = FunctionDefinition::new("run");
        let mut iterator =
            new_stack_iterator(&[StackFrame::new(def.clone(), vec![1, 2], vec![3], 7, 11)]);
        listener.before(&ctx, &module, &def, &[1, 2], &mut iterator);
        listener.after(&ctx, &module, &def, &[3]);
        listener.abort(&ctx, &module, &def, &RuntimeError::new("boom"));

        assert_eq!(
            vec!["closure:run:run@7:11"],
            *calls.lock().expect("calls poisoned")
        );
    }
}
