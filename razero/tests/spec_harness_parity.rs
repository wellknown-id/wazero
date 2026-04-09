use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use razero::{
    experimental::{CORE_FEATURES_EXTENDED_CONST, CORE_FEATURES_TAIL_CALL, CORE_FEATURES_THREADS},
    CoreFeatures, GlobalValue, Module, ModuleConfig, Runtime, RuntimeConfig, ValueType,
};
use serde::Deserialize;
use serde_json::Value;

const F32_CANONICAL_NAN_BITS: u32 = 0x7fc0_0000;
const F32_CANONICAL_NAN_BITS_MASK: u32 = 0x7fff_ffff;
const F32_ARITHMETIC_NAN_PAYLOAD_MSB: u32 = 0x0040_0000;
const F32_EXPONENT_MASK: u32 = 0x7f80_0000;
const F32_ARITHMETIC_NAN_BITS: u32 = F32_CANONICAL_NAN_BITS | 0b1;

const F64_CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const F64_CANONICAL_NAN_BITS_MASK: u64 = 0x7fff_ffff_ffff_ffff;
const F64_ARITHMETIC_NAN_PAYLOAD_MSB: u64 = 0x0008_0000_0000_0000;
const F64_EXPONENT_MASK: u64 = 0x7ff0_0000_0000_0000;
const F64_ARITHMETIC_NAN_BITS: u64 = F64_CANONICAL_NAN_BITS | 0b1;

const SPECTEST_WASM: &[u8] =
    include_bytes!("../../internal/integration_test/spectest/testdata/spectest.wasm");
const ALIGN_V1_69_WASM: &[u8] =
    include_bytes!("../../internal/integration_test/spectest/v1/testdata/align.69.wasm");
const ALIGN_V1_105_WASM: &[u8] =
    include_bytes!("../../internal/integration_test/spectest/v1/testdata/align.105.wasm");
const ALIGN_V2_69_WASM: &[u8] =
    include_bytes!("../../internal/integration_test/spectest/v2/testdata/align.69.wasm");
const ALIGN_V2_105_WASM: &[u8] =
    include_bytes!("../../internal/integration_test/spectest/v2/testdata/align.105.wasm");
const RETURN_CALL_INDIRECT_12_WASM: &[u8] = include_bytes!(
    "../../internal/integration_test/spectest/tail-call/testdata/return_call_indirect.12.wasm"
);
const RETURN_CALL_INDIRECT_13_WASM: &[u8] = include_bytes!(
    "../../internal/integration_test/spectest/tail-call/testdata/return_call_indirect.13.wasm"
);

#[derive(Debug, Deserialize)]
struct TestCase {
    #[serde(rename = "source_filename")]
    source_file: String,
    commands: Vec<Command>,
}

#[derive(Debug, Deserialize)]
struct Command {
    #[serde(rename = "type")]
    command_type: String,
    line: usize,
    #[serde(default)]
    name: String,
    #[serde(default)]
    filename: String,
    #[serde(default, rename = "as")]
    as_name: String,
    #[serde(default)]
    action: CommandAction,
    #[serde(default, rename = "expected")]
    expected: Vec<CommandActionValue>,
    #[serde(default, rename = "module_type")]
    module_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct CommandAction {
    #[serde(rename = "type", default)]
    action_type: String,
    #[serde(default)]
    args: Vec<CommandActionValue>,
    #[serde(default)]
    field: String,
    #[serde(default)]
    module: String,
}

#[derive(Debug, Deserialize)]
struct CommandActionValue {
    #[serde(rename = "type")]
    val_type: String,
    #[serde(default, rename = "lane_type")]
    lane_type: String,
    #[serde(default)]
    value: Value,
}

impl CommandActionValue {
    fn scalar(&self) -> &str {
        self.value
            .as_str()
            .unwrap_or_else(|| panic!("expected scalar string value for {}", self.val_type))
    }

    fn lanes(&self) -> &[Value] {
        self.value
            .as_array()
            .unwrap_or_else(|| panic!("expected lane array for {}", self.val_type))
    }

    fn to_bits_vec(&self) -> Vec<u64> {
        if self.val_type == "v128" {
            let (lo, hi) = build_lane_u64s(self.lanes(), &self.lane_type);
            vec![lo, hi]
        } else {
            vec![self.to_u64()]
        }
    }

    fn to_u64(&self) -> u64 {
        let value = self.scalar();
        if value.contains("nan") {
            return get_nan_bits(value, self.val_type == "f32");
        }
        match self.val_type.as_str() {
            "externref" => {
                if value == "null" {
                    0
                } else {
                    value.parse::<u64>().unwrap() + 1
                }
            }
            "funcref" => 0,
            "i32" | "f32" => value.parse::<u32>().unwrap() as u64,
            _ => value.parse::<u64>().unwrap(),
        }
    }

    fn to_global_value(&self) -> GlobalValue {
        match self.val_type.as_str() {
            "i32" => GlobalValue::I32(self.to_u64() as u32 as i32),
            "i64" => GlobalValue::I64(self.to_u64() as i64),
            "f32" => GlobalValue::F32(self.to_u64() as u32),
            "f64" => GlobalValue::F64(self.to_u64()),
            other => panic!("unsupported global value type {other}"),
        }
    }
}

#[test]
fn spectest_v1_suite() {
    run_suite(
        "v1",
        "../internal/integration_test/spectest/v1/testdata",
        CoreFeatures::V1,
    );
}

#[test]
fn spectest_v2_suite() {
    run_suite(
        "v2",
        "../internal/integration_test/spectest/v2/testdata",
        CoreFeatures::V2,
    );
}

#[test]
fn spectest_threads_suite() {
    run_suite(
        "threads",
        "../internal/integration_test/spectest/threads/testdata",
        CoreFeatures::V2 | CORE_FEATURES_THREADS,
    );
}

#[test]
fn spectest_tail_call_suite() {
    run_suite(
        "tail-call",
        "../internal/integration_test/spectest/tail-call/testdata",
        CoreFeatures::V2 | CORE_FEATURES_TAIL_CALL,
    );
}

#[test]
fn spectest_extended_const_suite() {
    run_suite(
        "extended-const",
        "../internal/integration_test/spectest/extended-const/testdata",
        CoreFeatures::V2 | CORE_FEATURES_EXTENDED_CONST,
    );
}

#[test]
fn spectest_extended_const_data_regression() {
    run_case(
        "extended-const",
        &repo_path("../internal/integration_test/spectest/extended-const/testdata"),
        "data",
        CoreFeatures::V2 | CORE_FEATURES_EXTENDED_CONST,
    );
}

#[test]
fn spectest_extended_const_elem_regression() {
    run_case(
        "extended-const",
        &repo_path("../internal/integration_test/spectest/extended-const/testdata"),
        "elem",
        CoreFeatures::V2 | CORE_FEATURES_EXTENDED_CONST,
    );
}

#[test]
fn spectest_tail_call_return_call_regression() {
    run_case(
        "tail-call",
        &repo_path("../internal/integration_test/spectest/tail-call/testdata"),
        "return_call",
        CoreFeatures::V2 | CORE_FEATURES_TAIL_CALL,
    );
}

#[test]
fn spectest_tail_call_return_call_indirect_unknown_table_regression() {
    let runtime = Runtime::with_config(
        RuntimeConfig::new().with_core_features(CoreFeatures::V2 | CORE_FEATURES_TAIL_CALL),
    );
    let err = runtime
        .instantiate_binary(RETURN_CALL_INDIRECT_12_WASM, ModuleConfig::new())
        .err()
        .unwrap_or_else(|| panic!("return_call_indirect.12.wasm should fail validation"));
    assert!(
        err.to_string().contains("unknown table"),
        "unexpected error for return_call_indirect.12.wasm: {err}"
    );
}

#[test]
fn spectest_tail_call_return_call_indirect_type_mismatch_regression() {
    let runtime = Runtime::with_config(
        RuntimeConfig::new().with_core_features(CoreFeatures::V2 | CORE_FEATURES_TAIL_CALL),
    );
    let err = runtime
        .instantiate_binary(RETURN_CALL_INDIRECT_13_WASM, ModuleConfig::new())
        .err()
        .unwrap_or_else(|| panic!("return_call_indirect.13.wasm should fail validation"));
    assert!(
        err.to_string().contains("type mismatch") || err.to_string().contains("stack underflow"),
        "unexpected error for return_call_indirect.13.wasm: {err}"
    );
}

#[test]
fn spectest_threads_atomic_regression() {
    run_case(
        "threads",
        &repo_path("../internal/integration_test/spectest/threads/testdata"),
        "atomic",
        CoreFeatures::V2 | CORE_FEATURES_THREADS,
    );
}

#[test]
fn spectest_v1_memory_regression() {
    run_case(
        "v1",
        &repo_path("../internal/integration_test/spectest/v1/testdata"),
        "memory",
        CoreFeatures::V1,
    );
}

#[test]
fn spectest_v2_memory_regression() {
    run_case(
        "v2",
        &repo_path("../internal/integration_test/spectest/v2/testdata"),
        "memory",
        CoreFeatures::V2,
    );
}

#[test]
fn rejects_invalid_spec_alignment_modules() {
    for (name, features, bytes) in [
        ("v1 align.69", CoreFeatures::V1, ALIGN_V1_69_WASM),
        ("v1 align.105", CoreFeatures::V1, ALIGN_V1_105_WASM),
        ("v2 align.69", CoreFeatures::V2, ALIGN_V2_69_WASM),
        ("v2 align.105", CoreFeatures::V2, ALIGN_V2_105_WASM),
    ] {
        let runtime = Runtime::with_config(RuntimeConfig::new().with_core_features(features));
        assert!(
            runtime
                .instantiate_binary(bytes, ModuleConfig::new())
                .is_err(),
            "{name} unexpectedly instantiated"
        );
    }
}

fn run_suite(name: &str, relative_testdata_dir: &str, features: CoreFeatures) {
    let testdata_dir = repo_path(relative_testdata_dir);
    let case_names = list_case_names(&testdata_dir);
    assert!(
        !case_names.is_empty(),
        "no spectest JSON fixtures found for {name} in {}",
        testdata_dir.display()
    );
    for case_name in case_names {
        run_case(name, &testdata_dir, &case_name, features);
    }
}

fn run_case(suite_name: &str, testdata_dir: &Path, case_name: &str, features: CoreFeatures) {
    let raw =
        fs::read_to_string(testdata_dir.join(format!("{case_name}.json"))).unwrap_or_else(|err| {
            panic!("{suite_name}/{case_name}: failed to read JSON fixture: {err}")
        });
    let testcase: TestCase = serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("{suite_name}/{case_name}: failed to decode JSON fixture: {err}")
    });
    let wast_name = basename(&testcase.source_file);

    let runtime = Runtime::with_config(RuntimeConfig::new().with_core_features(features));
    runtime
        .instantiate_binary(SPECTEST_WASM, ModuleConfig::new().with_name("spectest"))
        .unwrap_or_else(|err| {
            panic!("{suite_name}/{case_name}: failed to instantiate spectest import module: {err}")
        });

    let mut modules = BTreeMap::<String, Module>::new();
    let mut last_instantiated_module = None::<Module>;
    let mut i = 0;
    while i < testcase.commands.len() {
        let command = &testcase.commands[i];
        let context = format!(
            "{suite_name}/{wast_name}:{} {}",
            command.line, command.command_type
        );
        match command.command_type.as_str() {
            "module" => {
                let bytes = read_fixture_bytes(testdata_dir, &command.filename, &context);
                let registered_name = testcase
                    .commands
                    .get(i + 1)
                    .filter(|next| next.command_type == "register")
                    .map(|next| next.as_name.as_str())
                    .unwrap_or("");
                if !registered_name.is_empty() {
                    i += 1;
                }
                let mut config = ModuleConfig::new();
                if !registered_name.is_empty() {
                    config = config.with_name(registered_name);
                }
                let module = runtime
                    .instantiate_binary(&bytes, config)
                    .unwrap_or_else(|err| {
                        panic!(
                            "{context}: failed to instantiate module {}: {err}",
                            command.filename
                        )
                    });
                if !command.name.is_empty() {
                    modules.insert(command.name.clone(), module.clone());
                }
                last_instantiated_module = Some(module);
            }
            "assert_return" | "action" => {
                let module = action_module(
                    &context,
                    &modules,
                    last_instantiated_module.as_ref(),
                    command,
                );
                match command.action.action_type.as_str() {
                    "invoke" => {
                        let function = module
                            .exported_function(&command.action.field)
                            .unwrap_or_else(|| {
                                panic!("{context}: missing export {}", command.action.field)
                            });
                        let args = flatten_bits(&command.action.args);
                        let expected = flatten_bits(&command.expected);
                        let results = function.call(&args).unwrap_or_else(|err| {
                            panic!(
                                "{context}: invoke {}({:?}) failed: {err}",
                                command.action.field, command.action.args
                            )
                        });
                        assert_eq!(
                            expected.len(),
                            results.len(),
                            "{context}: result count mismatch for {}",
                            command.action.field
                        );
                        let lane_types = command
                            .expected
                            .iter()
                            .enumerate()
                            .filter_map(|(index, value)| {
                                (value.val_type == "v128")
                                    .then_some((index, value.lane_type.as_str()))
                            })
                            .collect::<BTreeMap<_, _>>();
                        let (matched, values_msg) = values_eq(
                            &results,
                            &expected,
                            function.definition().result_types(),
                            &lane_types,
                        );
                        assert!(
                            matched,
                            "{context}: value mismatch for {}{}\n{}",
                            command.action.field,
                            action_module_suffix(command),
                            values_msg
                        );
                    }
                    "get" => {
                        assert_eq!(
                            1,
                            command.expected.len(),
                            "{context}: get expects one value"
                        );
                        let global = module
                            .exported_global(&command.action.field)
                            .unwrap_or_else(|| {
                                panic!("{context}: missing global export {}", command.action.field)
                            });
                        let expected = command.expected[0].to_global_value();
                        assert_eq!(
                            expected,
                            global.get(),
                            "{context}: global {} mismatch{}",
                            command.action.field,
                            action_module_suffix(command)
                        );
                    }
                    other => panic!("{context}: unsupported action type {other}"),
                }
            }
            "assert_malformed" => {
                if command.module_type != "text" {
                    let bytes = read_fixture_bytes(testdata_dir, &command.filename, &context);
                    assert!(
                        runtime
                            .instantiate_binary(&bytes, ModuleConfig::new())
                            .is_err(),
                        "{context}: malformed binary fixture {} unexpectedly instantiated",
                        command.filename
                    );
                }
            }
            "assert_trap" => {
                let module = action_module(
                    &context,
                    &modules,
                    last_instantiated_module.as_ref(),
                    command,
                );
                match command.action.action_type.as_str() {
                    "invoke" => {
                        let args = flatten_bits(&command.action.args);
                        let err = module
                            .exported_function(&command.action.field)
                            .unwrap_or_else(|| {
                                panic!("{context}: missing export {}", command.action.field)
                            })
                            .call(&args)
                            .unwrap_err();
                        let expected = expected_trap_text(&command.text);
                        assert_eq!(
                            expected,
                            err.to_string(),
                            "{context}: trap mismatch for {}{}",
                            command.action.field,
                            action_module_suffix(command)
                        );
                    }
                    other => panic!("{context}: unsupported trap action type {other}"),
                }
            }
            "assert_invalid" => {
                if command.module_type != "text" {
                    let bytes = read_fixture_bytes(testdata_dir, &command.filename, &context);
                    assert!(
                        runtime
                            .instantiate_binary(&bytes, ModuleConfig::new())
                            .is_err(),
                        "{context}: invalid binary fixture {} unexpectedly instantiated",
                        command.filename
                    );
                }
            }
            "assert_exhaustion" => match command.action.action_type.as_str() {
                "invoke" => {
                    let module = last_instantiated_module.as_ref().unwrap_or_else(|| {
                        panic!("{context}: no module available for exhaustion assertion")
                    });
                    let args = flatten_bits(&command.action.args);
                    let err = module
                        .exported_function(&command.action.field)
                        .unwrap_or_else(|| {
                            panic!("{context}: missing export {}", command.action.field)
                        })
                        .call(&args)
                        .unwrap_err();
                    assert_eq!(
                        "stack overflow",
                        err.to_string(),
                        "{context}: exhaustion mismatch for {}",
                        command.action.field
                    );
                }
                other => panic!("{context}: unsupported exhaustion action type {other}"),
            },
            "assert_unlinkable" => {
                if command.module_type != "text" {
                    let bytes = read_fixture_bytes(testdata_dir, &command.filename, &context);
                    assert!(
                        runtime
                            .instantiate_binary(&bytes, ModuleConfig::new())
                            .is_err(),
                        "{context}: unlinkable binary fixture {} unexpectedly instantiated",
                        command.filename
                    );
                }
            }
            "assert_uninstantiable" => {
                let bytes = read_fixture_bytes(testdata_dir, &command.filename, &context);
                let result = runtime.instantiate_binary(&bytes, ModuleConfig::new());
                if command.text == "out of bounds table access" {
                    assert!(
                        result.is_ok(),
                        "{context}: expected Go-compatible success for {} but got {:?}",
                        command.filename,
                        result.err().map(|err| err.to_string())
                    );
                } else {
                    assert!(
                        result.is_err(),
                        "{context}: uninstantiable fixture {} unexpectedly instantiated",
                        command.filename
                    );
                }
            }
            "register" => panic!("{context}: unexpected standalone register command"),
            other => panic!("{context}: unsupported command type {other}"),
        }
        i += 1;
    }
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn list_case_names(testdata_dir: &Path) -> Vec<String> {
    let mut names = fs::read_dir(testdata_dir)
        .unwrap_or_else(|err| panic!("failed to list {}: {err}", testdata_dir.display()))
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("json"))
                .then(|| path.file_stem()?.to_str().map(ToOwned::to_owned))
                .flatten()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn read_fixture_bytes(testdata_dir: &Path, filename: &str, context: &str) -> Vec<u8> {
    fs::read(testdata_dir.join(filename))
        .unwrap_or_else(|err| panic!("{context}: failed to read fixture {filename}: {err}"))
}

fn action_module<'a>(
    context: &str,
    modules: &'a BTreeMap<String, Module>,
    last_instantiated_module: Option<&'a Module>,
    command: &Command,
) -> &'a Module {
    if !command.action.module.is_empty() {
        modules
            .get(&command.action.module)
            .unwrap_or_else(|| panic!("{context}: missing module {}", command.action.module))
    } else {
        last_instantiated_module.unwrap_or_else(|| panic!("{context}: no current module available"))
    }
}

fn action_module_suffix(command: &Command) -> String {
    if command.action.module.is_empty() {
        String::new()
    } else {
        format!(" in module {}", command.action.module)
    }
}

fn flatten_bits(values: &[CommandActionValue]) -> Vec<u64> {
    values
        .iter()
        .flat_map(CommandActionValue::to_bits_vec)
        .collect()
}

fn expected_trap_text(text: &str) -> &'static str {
    match text {
        "expected shared memory" => "expected shared memory",
        "out of bounds memory access" => "out of bounds memory access",
        "indirect call type mismatch" | "indirect call" => "indirect call type mismatch",
        "undefined element" | "undefined" | "out of bounds table access" => "invalid table access",
        "integer overflow" => "integer overflow",
        "invalid conversion to integer" => "invalid conversion to integer",
        "integer divide by zero" => "integer divide by zero",
        "unaligned atomic" => "unaligned atomic",
        "unreachable" => "unreachable",
        value if value.starts_with("uninitialized") => "invalid table access",
        other => panic!("unsupported spectest trap text {other}"),
    }
}

fn build_lane_u64s(values: &[Value], lane_type: &str) -> (u64, u64) {
    let (width, lane_count) = match lane_type {
        "i8" => (8, 16),
        "i16" => (16, 8),
        "i32" | "f32" => (32, 4),
        "i64" | "f64" => (64, 2),
        other => panic!("unsupported lane type {other}"),
    };
    let mut lo = 0_u64;
    let mut hi = 0_u64;
    for (index, raw) in values.iter().enumerate().take(lane_count) {
        let raw = raw
            .as_str()
            .unwrap_or_else(|| panic!("lane {index} in {lane_type} should be string"));
        let value = if raw.contains("nan") {
            get_nan_bits(raw, width == 32)
        } else {
            raw.parse::<u64>().unwrap()
        };
        if index < lane_count / 2 {
            lo |= value << (index * width);
        } else {
            hi |= value << ((index - lane_count / 2) * width);
        }
    }
    (lo, hi)
}

fn get_nan_bits(value: &str, is_32_bit: bool) -> u64 {
    match (is_32_bit, value) {
        (true, "nan:canonical") => F32_CANONICAL_NAN_BITS as u64,
        (true, "nan:arithmetic") => F32_ARITHMETIC_NAN_BITS as u64,
        (false, "nan:canonical") => F64_CANONICAL_NAN_BITS,
        (false, "nan:arithmetic") => F64_ARITHMETIC_NAN_BITS,
        _ => panic!("unsupported NaN literal {value}"),
    }
}

fn values_eq(
    actual: &[u64],
    expected: &[u64],
    value_types: &[ValueType],
    lane_types: &BTreeMap<usize, &str>,
) -> (bool, String) {
    let mut matched = true;
    let mut expected_strings = Vec::with_capacity(value_types.len());
    let mut actual_strings = Vec::with_capacity(value_types.len());
    let mut index = 0;

    for (value_index, value_type) in value_types.iter().enumerate() {
        match value_type {
            ValueType::I32 => {
                expected_strings.push(format!("{}", expected[index] as u32));
                actual_strings.push(format!("{}", actual[index] as u32));
                matched &= expected[index] as u32 == actual[index] as u32;
                index += 1;
            }
            ValueType::I64 | ValueType::ExternRef | ValueType::FuncRef => {
                expected_strings.push(expected[index].to_string());
                actual_strings.push(actual[index].to_string());
                matched &= expected[index] == actual[index];
                index += 1;
            }
            ValueType::F32 => {
                let expected_value = f32::from_bits(expected[index] as u32);
                let actual_value = f32::from_bits(actual[index] as u32);
                expected_strings.push(format_go_f32(expected_value));
                actual_strings.push(format_go_f32(actual_value));
                matched &= f32_equal(expected_value, actual_value);
                index += 1;
            }
            ValueType::F64 => {
                let expected_value = f64::from_bits(expected[index]);
                let actual_value = f64::from_bits(actual[index]);
                expected_strings.push(format_go_f64(expected_value));
                actual_strings.push(format_go_f64(actual_value));
                matched &= f64_equal(expected_value, actual_value);
                index += 1;
            }
            ValueType::V128 => {
                let lane_type = lane_types
                    .get(&value_index)
                    .copied()
                    .unwrap_or_else(|| panic!("missing lane type for v128 result {value_index}"));
                let expected_lo = expected[index];
                let expected_hi = expected[index + 1];
                let actual_lo = actual[index];
                let actual_hi = actual[index + 1];
                expected_strings.push(format_v128(expected_lo, expected_hi, lane_type));
                actual_strings.push(format_v128(actual_lo, actual_hi, lane_type));
                matched &= v128_equal(expected_lo, expected_hi, actual_lo, actual_hi, lane_type);
                index += 2;
            }
        }
    }

    if matched {
        (true, String::new())
    } else {
        (
            false,
            format!(
                "\thave [{}]\n\twant [{}]",
                actual_strings.join(", "),
                expected_strings.join(", "),
            ),
        )
    }
}

fn v128_equal(
    expected_lo: u64,
    expected_hi: u64,
    actual_lo: u64,
    actual_hi: u64,
    lane_type: &str,
) -> bool {
    match lane_type {
        "i8" | "i16" | "i32" | "i64" => expected_lo == actual_lo && expected_hi == actual_hi,
        "f32" => {
            f32_equal(
                f32::from_bits(expected_lo as u32),
                f32::from_bits(actual_lo as u32),
            ) && f32_equal(
                f32::from_bits((expected_lo >> 32) as u32),
                f32::from_bits((actual_lo >> 32) as u32),
            ) && f32_equal(
                f32::from_bits(expected_hi as u32),
                f32::from_bits(actual_hi as u32),
            ) && f32_equal(
                f32::from_bits((expected_hi >> 32) as u32),
                f32::from_bits((actual_hi >> 32) as u32),
            )
        }
        "f64" => {
            f64_equal(f64::from_bits(expected_lo), f64::from_bits(actual_lo))
                && f64_equal(f64::from_bits(expected_hi), f64::from_bits(actual_hi))
        }
        other => panic!("unsupported lane type {other}"),
    }
}

fn format_v128(lo: u64, hi: u64, lane_type: &str) -> String {
    match lane_type {
        "i8" => format!(
            "i8x16({})",
            [
                lo as u8 as u64,
                (lo >> 8) as u8 as u64,
                (lo >> 16) as u8 as u64,
                (lo >> 24) as u8 as u64,
                (lo >> 32) as u8 as u64,
                (lo >> 40) as u8 as u64,
                (lo >> 48) as u8 as u64,
                (lo >> 56) as u8 as u64,
                hi as u8 as u64,
                (hi >> 8) as u8 as u64,
                (hi >> 16) as u8 as u64,
                (hi >> 24) as u8 as u64,
                (hi >> 32) as u8 as u64,
                (hi >> 40) as u8 as u64,
                (hi >> 48) as u8 as u64,
                (hi >> 56) as u8 as u64,
            ]
            .into_iter()
            .map(|value| format!("{value:#x}"))
            .collect::<Vec<_>>()
            .join(", ")
        ),
        "i16" => format!(
            "i16x8({})",
            [
                lo as u16 as u64,
                (lo >> 16) as u16 as u64,
                (lo >> 32) as u16 as u64,
                (lo >> 48) as u16 as u64,
                hi as u16 as u64,
                (hi >> 16) as u16 as u64,
                (hi >> 32) as u16 as u64,
                (hi >> 48) as u16 as u64,
            ]
            .into_iter()
            .map(|value| format!("{value:#x}"))
            .collect::<Vec<_>>()
            .join(", ")
        ),
        "i32" => format!(
            "i32x4({})",
            [
                lo as u32 as u64,
                (lo >> 32) as u32 as u64,
                hi as u32 as u64,
                (hi >> 32) as u32 as u64,
            ]
            .into_iter()
            .map(|value| format!("{value:#x}"))
            .collect::<Vec<_>>()
            .join(", ")
        ),
        "i64" => format!(
            "i64x2({})",
            [lo, hi]
                .into_iter()
                .map(|value| format!("{value:#x}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "f32" => format!(
            "f32x4({}, {}, {}, {})",
            format_go_f32(f32::from_bits(lo as u32)),
            format_go_f32(f32::from_bits((lo >> 32) as u32)),
            format_go_f32(f32::from_bits(hi as u32)),
            format_go_f32(f32::from_bits((hi >> 32) as u32)),
        ),
        "f64" => format!(
            "f64x2({}, {})",
            format_go_f64(f64::from_bits(lo)),
            format_go_f64(f64::from_bits(hi)),
        ),
        other => panic!("unsupported lane type {other}"),
    }
}

fn format_go_f32(value: f32) -> String {
    format_go_f64(value as f64)
}

fn format_go_f64(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "+Inf".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Inf".to_string()
    } else {
        format!("{value:.6}")
    }
}

fn f32_equal(expected: f32, actual: f32) -> bool {
    let expected_bits = expected.to_bits();
    if expected_bits == F32_CANONICAL_NAN_BITS {
        actual.to_bits() & F32_CANONICAL_NAN_BITS_MASK == F32_CANONICAL_NAN_BITS
    } else if expected_bits == F32_ARITHMETIC_NAN_BITS {
        let actual_bits = actual.to_bits();
        actual_bits & F32_EXPONENT_MASK == F32_EXPONENT_MASK
            && actual_bits & F32_ARITHMETIC_NAN_PAYLOAD_MSB == F32_ARITHMETIC_NAN_PAYLOAD_MSB
    } else if expected.is_nan() {
        actual.is_nan()
    } else {
        expected.to_bits() == actual.to_bits()
    }
}

fn f64_equal(expected: f64, actual: f64) -> bool {
    let expected_bits = expected.to_bits();
    if expected_bits == F64_CANONICAL_NAN_BITS {
        actual.to_bits() & F64_CANONICAL_NAN_BITS_MASK == F64_CANONICAL_NAN_BITS
    } else if expected_bits == F64_ARITHMETIC_NAN_BITS {
        let actual_bits = actual.to_bits();
        actual_bits & F64_EXPONENT_MASK == F64_EXPONENT_MASK
            && actual_bits & F64_ARITHMETIC_NAN_PAYLOAD_MSB == F64_ARITHMETIC_NAN_PAYLOAD_MSB
    } else if expected.is_nan() {
        actual.is_nan()
    } else {
        expected.to_bits() == actual.to_bits()
    }
}

#[test]
fn helper_f32_nan_equality_matches_go_rules() {
    assert!(f32_equal(
        f32::from_bits(F32_CANONICAL_NAN_BITS),
        f32::from_bits(F32_CANONICAL_NAN_BITS),
    ));
    assert!(!f32_equal(
        f32::from_bits(F32_CANONICAL_NAN_BITS),
        f32::from_bits(F32_ARITHMETIC_NAN_BITS),
    ));
    assert!(f32_equal(
        f32::from_bits(F32_ARITHMETIC_NAN_BITS),
        f32::from_bits(F32_ARITHMETIC_NAN_BITS | (1 << 2)),
    ));
    assert!(!f32_equal(-0.0, 0.0));
}

#[test]
fn helper_values_eq_matches_v128_lane_semantics() {
    let (matched, values_msg) = values_eq(
        &[u64::MAX, 0xff_u64 << 48 | 0xcc],
        &[0, 0xff_u64 << 56 | 0xaa],
        &[ValueType::V128],
        &BTreeMap::from([(0, "i8")]),
    );
    assert!(!matched);
    assert_eq!(
        "\thave [i8x16(0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xcc, 0x0, 0x0, 0x0, 0x0, 0x0, 0xff, 0x0)]\n\twant [i8x16(0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0xaa, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0xff)]",
        values_msg
    );
}

#[test]
fn helper_json_value_decoding_matches_go_command_semantics() {
    let value: CommandActionValue = serde_json::from_str(
        r#"{"type":"v128","lane_type":"f32","value":["nan:canonical","nan:arithmetic","nan:canonical","nan:arithmetic"]}"#,
    )
    .unwrap();
    assert_eq!(
        vec![
            (F32_ARITHMETIC_NAN_BITS as u64) << 32 | F32_CANONICAL_NAN_BITS as u64,
            (F32_ARITHMETIC_NAN_BITS as u64) << 32 | F32_CANONICAL_NAN_BITS as u64,
        ],
        value.to_bits_vec()
    );

    let externref: CommandActionValue =
        serde_json::from_str(r#"{"type":"externref","value":"0"}"#).unwrap();
    assert_eq!(1, externref.to_u64());
}
