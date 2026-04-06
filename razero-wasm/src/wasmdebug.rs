use std::any::Any;
use std::backtrace::Backtrace;
use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt::{Display, Formatter};

use crate::wasmruntime;

pub trait ValueTypeName {
    fn value_type_name(&self) -> &'static str;
}

pub fn func_name(module_name: &str, func_name: &str, func_idx: u32) -> String {
    let mut rendered = String::with_capacity(module_name.len() + func_name.len() + 8);
    rendered.push_str(module_name);
    rendered.push('.');
    if func_name.is_empty() {
        rendered.push('$');
        rendered.push_str(&func_idx.to_string());
    } else {
        rendered.push_str(func_name);
    }
    rendered
}

pub fn signature<V: ValueTypeName>(
    func_name: &str,
    param_types: &[V],
    result_types: &[V],
) -> String {
    let mut rendered = String::from(func_name);
    rendered.push('(');
    for (index, param) in param_types.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        rendered.push_str(param.value_type_name());
    }
    rendered.push(')');

    match result_types {
        [] => {}
        [result] => {
            rendered.push(' ');
            rendered.push_str(result.value_type_name());
        }
        _ => {
            rendered.push(' ');
            rendered.push('(');
            for (index, result) in result_types.iter().enumerate() {
                if index > 0 {
                    rendered.push(',');
                }
                rendered.push_str(result.value_type_name());
            }
            rendered.push(')');
        }
    }

    rendered
}

pub const GO_RUNTIME_ERROR_TRACE_PREFIX: &str = "Go runtime stack trace:";
pub const MAX_FRAMES: usize = 30;

#[derive(Debug, Default)]
pub struct ErrorBuilder {
    frame_count: usize,
    lines: Vec<String>,
}

pub fn new_error_builder() -> ErrorBuilder {
    ErrorBuilder::new()
}

impl ErrorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn add_frame<V, I, S>(
        &mut self,
        func_name: &str,
        param_types: &[V],
        result_types: &[V],
        sources: I,
    ) where
        V: ValueTypeName,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if self.frame_count == MAX_FRAMES {
            return;
        }

        self.frame_count += 1;
        self.lines
            .push(signature(func_name, param_types, result_types));
        for source in sources {
            self.lines.push(format!("\t{}", source.as_ref()));
        }

        if self.frame_count == MAX_FRAMES {
            self.lines
                .push(String::from("... maybe followed by omitted frames"));
        }
    }

    pub fn from_wasm_error(self, recovered: wasmruntime::Error) -> TracedError {
        TracedError::new(Recovered::Wasm(recovered), self.lines)
    }

    pub fn from_host_error<E>(self, recovered: E) -> TracedError
    where
        E: StdError + Send + Sync + 'static,
    {
        TracedError::new(Recovered::Host(Box::new(recovered)), self.lines)
    }

    pub fn from_message(self, recovered: impl Into<String>) -> TracedError {
        TracedError::new(Recovered::Message(recovered.into()), self.lines)
    }

    pub fn from_runtime_fault(self, recovered: RuntimeFault) -> TracedError {
        TracedError::new(Recovered::RuntimeFault(recovered), self.lines)
    }

    pub fn from_panic(self, recovered: Box<dyn Any + Send>) -> TracedError {
        TracedError::new(recover_panic(recovered), self.lines)
    }
}

#[derive(Debug)]
pub enum Recovered {
    Wasm(wasmruntime::Error),
    Host(Box<dyn StdError + Send + Sync>),
    Message(String),
    RuntimeFault(RuntimeFault),
}

#[derive(Debug)]
pub struct RuntimeFault {
    message: String,
    backtrace: Backtrace,
}

impl RuntimeFault {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            backtrace: Backtrace::force_capture(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn backtrace(&self) -> &Backtrace {
        &self.backtrace
    }
}

#[derive(Debug)]
pub struct TracedError {
    recovered: Recovered,
    stack: String,
}

impl TracedError {
    fn new(recovered: Recovered, lines: Vec<String>) -> Self {
        Self {
            recovered,
            stack: lines.join("\n\t"),
        }
    }

    pub fn recovered(&self) -> &Recovered {
        &self.recovered
    }

    pub fn stack_trace(&self) -> &str {
        &self.stack
    }
}

impl Display for TracedError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.recovered {
            Recovered::Wasm(err) => {
                write!(f, "wasm error: {err}\nwasm stack trace:\n\t{}", self.stack)
            }
            Recovered::Host(err) => write!(
                f,
                "{err} (recovered by wazero)\nwasm stack trace:\n\t{}",
                self.stack
            ),
            Recovered::Message(message) => write!(
                f,
                "{message} (recovered by wazero)\nwasm stack trace:\n\t{}",
                self.stack
            ),
            Recovered::RuntimeFault(fault) => write!(
                f,
                "{} (recovered by wazero)\nwasm stack trace:\n\t{}\n\n{}\n{}",
                fault.message(),
                self.stack,
                GO_RUNTIME_ERROR_TRACE_PREFIX,
                fault.backtrace()
            ),
        }
    }
}

impl StdError for TracedError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.recovered {
            Recovered::Wasm(err) => Some(err),
            Recovered::Host(err) => Some(err.as_ref()),
            Recovered::Message(_) | Recovered::RuntimeFault(_) => None,
        }
    }
}

pub fn recover_panic(recovered: Box<dyn Any + Send>) -> Recovered {
    let recovered = match recovered.downcast::<wasmruntime::Error>() {
        Ok(err) => return Recovered::Wasm(*err),
        Err(recovered) => recovered,
    };
    let recovered = match recovered.downcast::<RuntimeFault>() {
        Ok(err) => return Recovered::RuntimeFault(*err),
        Err(recovered) => recovered,
    };
    let recovered = match recovered.downcast::<Box<dyn StdError + Send + Sync>>() {
        Ok(err) => return Recovered::Host(*err),
        Err(recovered) => recovered,
    };
    let recovered = match recovered.downcast::<String>() {
        Ok(message) => return Recovered::Message(*message),
        Err(recovered) => recovered,
    };
    match recovered.downcast::<&'static str>() {
        Ok(message) => Recovered::Message((*message).to_string()),
        Err(_) => Recovered::Message(String::from("panic payload of unknown type")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLine {
    file: Cow<'static, str>,
    line: u64,
    column: u64,
    inlined: bool,
}

impl SourceLine {
    pub fn new(file: impl Into<Cow<'static, str>>, line: u64, column: u64) -> Self {
        Self {
            file: file.into(),
            line,
            column,
            inlined: false,
        }
    }

    pub fn with_inlined(mut self, inlined: bool) -> Self {
        self.inlined = inlined;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DwarfLineEntry {
    address: u64,
    lines: Vec<SourceLine>,
}

impl DwarfLineEntry {
    pub fn new(address: u64, lines: impl Into<Vec<SourceLine>>) -> Self {
        Self {
            address,
            lines: lines.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DWARFLines {
    entries: Vec<DwarfLineEntry>,
}

impl DWARFLines {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = DwarfLineEntry>) -> Self {
        let mut dwarf_lines = Self::new();
        for entry in entries {
            dwarf_lines.add_entry(entry);
        }
        dwarf_lines
    }

    pub fn entries(&self) -> &[DwarfLineEntry] {
        &self.entries
    }

    pub fn add_entry(&mut self, entry: DwarfLineEntry) {
        if is_tombstone_addr(entry.address) {
            return;
        }

        match self
            .entries
            .binary_search_by_key(&entry.address, |entry| entry.address)
        {
            Ok(index) => self.entries[index] = entry,
            Err(index) => self.entries.insert(index, entry),
        }
    }

    pub fn line(&self, instruction_offset: u64) -> Vec<String> {
        let index = match self
            .entries
            .binary_search_by_key(&instruction_offset, |entry| entry.address)
        {
            Ok(index) => index,
            Err(0) => return Vec::new(),
            Err(index) => index - 1,
        };

        let entry = &self.entries[index];
        let prefix = format!("0x{instruction_offset:x}: ");
        let continuation_prefix = " ".repeat(prefix.len());

        entry
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let current_prefix = if index == 0 {
                    prefix.as_str()
                } else {
                    continuation_prefix.as_str()
                };
                format_line(
                    current_prefix,
                    line.file.as_ref(),
                    line.line,
                    line.column,
                    line.inlined,
                )
            })
            .collect()
    }
}

pub fn is_tombstone_addr(addr: u64) -> bool {
    let addr32 = addr as u32;
    addr32 == u32::MAX || addr32 == u32::MAX - 1 || addr32 == 0
}

pub fn format_line(prefix: &str, file_name: &str, line: u64, col: u64, inlined: bool) -> String {
    let mut rendered = String::from(prefix);
    rendered.push_str(file_name);

    if line != 0 {
        rendered.push(':');
        rendered.push_str(&line.to_string());
        if col != 0 {
            rendered.push(':');
            rendered.push_str(&col.to_string());
        }
    }

    if inlined {
        rendered.push_str(" (inlined)");
    }

    rendered
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io;
    use std::panic::{self, panic_any};

    use super::*;

    #[derive(Clone, Copy)]
    enum TestValueType {
        I32,
        I64,
        F32,
        F64,
    }

    impl ValueTypeName for TestValueType {
        fn value_type_name(&self) -> &'static str {
            match self {
                Self::I32 => "i32",
                Self::I64 => "i64",
                Self::F32 => "f32",
                Self::F64 => "f64",
            }
        }
    }

    #[test]
    fn func_name_formats_like_go_helper() {
        assert_eq!(".$0", func_name("", "", 0));
        assert_eq!(".y", func_name("", "y", 0));
        assert_eq!("x.$255", func_name("x", "", 255));
        assert_eq!("x.y z", func_name("x", "y z", 0));
    }

    #[test]
    fn signature_formats_params_and_results() {
        use TestValueType::{F32, F64, I32, I64};

        assert_eq!("x.y()", signature::<TestValueType>("x.y", &[], &[]));
        assert_eq!("x.y(i32)", signature("x.y", &[I32], &[]));
        assert_eq!("x.y(i32,f64)", signature("x.y", &[I32, F64], &[]));
        assert_eq!("x.y() i64", signature("x.y", &[], &[I64]));
        assert_eq!(
            "x.y() (f32,i32,f64)",
            signature("x.y", &[], &[F32, I32, F64])
        );
        assert_eq!(
            "x.y(i64,f32) (i64,f32)",
            signature("x.y", &[I64, F32], &[I64, F32])
        );
    }

    #[test]
    fn error_builder_formats_host_and_wasm_errors() {
        let mut builder = new_error_builder();
        builder.add_frame(
            "wasi_snapshot_preview1.fd_write",
            &[TestValueType::I32, TestValueType::I32],
            &[TestValueType::I32],
            ["/src/runtime.rs:73:6"],
        );
        builder.add_frame::<TestValueType, _, _>("x.y", &[], &[], std::iter::empty::<&str>());

        let traced = builder.from_host_error(io::Error::other("invalid argument"));
        assert_eq!(
            "invalid argument (recovered by wazero)\nwasm stack trace:\n\twasi_snapshot_preview1.fd_write(i32,i32) i32\n\t\t/src/runtime.rs:73:6\n\tx.y()",
            traced.to_string()
        );

        let mut builder = new_error_builder();
        builder.add_frame::<TestValueType, _, _>("x.y", &[], &[], std::iter::empty::<&str>());
        let traced = builder.from_wasm_error(wasmruntime::ERR_RUNTIME_STACK_OVERFLOW);
        assert_eq!(
            "wasm error: stack overflow\nwasm stack trace:\n\tx.y()",
            traced.to_string()
        );
        assert_eq!(
            wasmruntime::ERR_RUNTIME_STACK_OVERFLOW.to_string(),
            traced.source().expect("missing source").to_string()
        );
    }

    #[test]
    fn error_builder_formats_runtime_faults() {
        let mut builder = new_error_builder();
        builder.add_frame::<TestValueType, _, _>("x.y", &[], &[], std::iter::empty::<&str>());

        let traced = builder.from_runtime_fault(RuntimeFault::new("index out of bounds"));
        let rendered = traced.to_string();
        assert!(rendered.contains("index out of bounds (recovered by wazero)"));
        assert!(rendered.contains("wasm stack trace:\n\tx.y()"));
        assert!(rendered.contains(GO_RUNTIME_ERROR_TRACE_PREFIX));
    }

    #[test]
    fn from_panic_decodes_common_payloads() {
        let payload =
            panic::catch_unwind(|| panic_any(wasmruntime::ERR_RUNTIME_UNREACHABLE)).unwrap_err();
        let traced = new_error_builder().from_panic(payload);
        assert_eq!(
            "wasm error: unreachable\nwasm stack trace:\n\t",
            traced.to_string()
        );

        let payload = panic::catch_unwind(|| {
            panic_any(Box::new(io::Error::other("boom")) as Box<dyn StdError + Send + Sync>)
        })
        .unwrap_err();
        let traced = new_error_builder().from_panic(payload);
        assert_eq!(
            "boom (recovered by wazero)\nwasm stack trace:\n\t",
            traced.to_string()
        );

        let payload = panic::catch_unwind(|| panic!("kaboom")).unwrap_err();
        let traced = new_error_builder().from_panic(payload);
        assert_eq!(
            "kaboom (recovered by wazero)\nwasm stack trace:\n\t",
            traced.to_string()
        );
    }

    #[test]
    fn add_frame_honors_max_frames() {
        let mut builder = new_error_builder();
        for _ in 0..(MAX_FRAMES + 10) {
            builder.add_frame::<TestValueType, _, _>("x.y", &[], &[], ["a.go:1:2", "b.go:3:4"]);
        }

        assert_eq!(MAX_FRAMES, builder.frame_count());
        assert_eq!(MAX_FRAMES * 3 + 1, builder.lines().len());
        assert_eq!(
            Some(&String::from("... maybe followed by omitted frames")),
            builder.lines().last()
        );
    }

    #[test]
    fn tombstone_addresses_match_go_rules() {
        assert!(is_tombstone_addr(u32::MAX as u64));
        assert!(is_tombstone_addr((u32::MAX - 1) as u64));
        assert!(is_tombstone_addr(1u64 << 32));
        assert!(!is_tombstone_addr(0x40));
    }

    #[test]
    fn dwarf_lines_return_exact_or_previous_entry() {
        let lines = DWARFLines::from_entries([
            DwarfLineEntry::new(
                0x80,
                vec![
                    SourceLine::new("zig/main.zig", 10, 5).with_inlined(true),
                    SourceLine::new("zig/main.zig", 6, 5).with_inlined(true),
                    SourceLine::new("zig/main.zig", 2, 5),
                ],
            ),
            DwarfLineEntry::new(0x20, vec![SourceLine::new("main.go", 4, 3)]),
            DwarfLineEntry::new(u32::MAX as u64, vec![SourceLine::new("ignored", 1, 1)]),
        ]);

        assert_eq!(vec![String::from("0x20: main.go:4:3")], lines.line(0x20));
        assert_eq!(
            vec![
                String::from("0x82: zig/main.zig:10:5 (inlined)"),
                String::from("      zig/main.zig:6:5 (inlined)"),
                String::from("      zig/main.zig:2:5"),
            ],
            lines.line(0x82)
        );
        assert!(lines.line(0x10).is_empty());
        assert_eq!(2, lines.entries().len());
    }
}
