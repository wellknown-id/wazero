use std::fmt::{self, Display, Formatter};
use std::io::{self, Write};
use std::ops::{BitOr, BitOrAssign};

use crate::api::wasm::{decode_f32, decode_f64, FunctionDefinition, Memory, ValueType};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

pub trait Logger: Send + Sync {
    fn log(&self, level: LogLevel, message: &str);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn log(&self, _level: LogLevel, _message: &str) {}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogScopes(u64);

impl LogScopes {
    pub const NONE: Self = Self(0);
    pub const CLOCK: Self = Self(1 << 0);
    pub const PROC: Self = Self(1 << 1);
    pub const FILESYSTEM: Self = Self(1 << 2);
    pub const MEMORY: Self = Self(1 << 3);
    pub const POLL: Self = Self(1 << 4);
    pub const RANDOM: Self = Self(1 << 5);
    pub const SOCK: Self = Self(1 << 6);
    pub const ALL: Self = Self(u64::MAX);

    pub const fn is_enabled(self, scope: Self) -> bool {
        self.0 & scope.0 != 0
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl BitOr for LogScopes {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for LogScopes {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl Display for LogScopes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if *self == Self::ALL {
            return f.write_str("all");
        }

        let mut wrote = false;
        for bit in 0..=63 {
            let target = Self(1_u64 << bit);
            if self.is_enabled(target) {
                if wrote {
                    f.write_str("|")?;
                }
                wrote = true;
                write!(f, "{}", scope_name(target))?;
            }
        }
        Ok(())
    }
}

fn scope_name(scope: LogScopes) -> ScopeName {
    let name = match scope {
        LogScopes::CLOCK => Some("clock"),
        LogScopes::PROC => Some("proc"),
        LogScopes::FILESYSTEM => Some("filesystem"),
        LogScopes::MEMORY => Some("memory"),
        LogScopes::POLL => Some("poll"),
        LogScopes::RANDOM => Some("random"),
        LogScopes::SOCK => Some("sock"),
        _ => None,
    };
    ScopeName {
        scope: scope.raw(),
        name,
    }
}

struct ScopeName {
    scope: u64,
    name: Option<&'static str>,
}

impl Display for ScopeName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.name {
            f.write_str(name)
        } else {
            write!(f, "<unknown={}>", self.scope)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogValueType {
    I32,
    I64,
    F32,
    F64,
    V128,
    FuncRef,
    ExternRef,
    MemI32,
    MemH64,
    String,
}

impl From<ValueType> for LogValueType {
    fn from(value_type: ValueType) -> Self {
        match value_type {
            ValueType::I32 => Self::I32,
            ValueType::I64 => Self::I64,
            ValueType::F32 => Self::F32,
            ValueType::F64 => Self::F64,
            ValueType::V128 => Self::V128,
            ValueType::ExternRef => Self::ExternRef,
            ValueType::FuncRef => Self::FuncRef,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParamLogger {
    offset_in_stack: usize,
    name: Option<String>,
    value_type: LogValueType,
}

impl ParamLogger {
    pub fn new(offset_in_stack: usize, name: impl Into<String>, value_type: LogValueType) -> Self {
        Self {
            offset_in_stack,
            name: Some(name.into()),
            value_type,
        }
    }

    pub fn unnamed(offset_in_stack: usize, value_type: LogValueType) -> Self {
        Self {
            offset_in_stack,
            name: None,
            value_type,
        }
    }

    pub fn log<W: Write>(
        &self,
        memory: Option<&Memory>,
        writer: &mut W,
        params: &[u64],
    ) -> io::Result<()> {
        if let Some(name) = &self.name {
            writer.write_all(name.as_bytes())?;
            writer.write_all(b"=")?;
        }
        self.value_type
            .write(memory, writer, self.offset_in_stack, params)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultLogger {
    offset_in_stack: usize,
    name: Option<String>,
    value_type: LogValueType,
}

impl ResultLogger {
    pub fn new(offset_in_stack: usize, name: impl Into<String>, value_type: LogValueType) -> Self {
        Self {
            offset_in_stack,
            name: Some(name.into()),
            value_type,
        }
    }

    pub fn unnamed(offset_in_stack: usize, value_type: LogValueType) -> Self {
        Self {
            offset_in_stack,
            name: None,
            value_type,
        }
    }

    pub fn log<W: Write>(
        &self,
        memory: Option<&Memory>,
        writer: &mut W,
        results: &[u64],
    ) -> io::Result<()> {
        if let Some(name) = &self.name {
            writer.write_all(name.as_bytes())?;
            writer.write_all(b"=")?;
        }
        self.value_type
            .write(memory, writer, self.offset_in_stack, results)
    }
}

pub fn config(function: &FunctionDefinition) -> (Vec<ParamLogger>, Vec<ResultLogger>) {
    let param_loggers = build_param_loggers(function.param_types(), function.param_names());
    let result_loggers = build_result_loggers(function.result_types(), function.result_names());
    (param_loggers, result_loggers)
}

fn build_param_loggers(types: &[ValueType], names: &[String]) -> Vec<ParamLogger> {
    let mut loggers = Vec::with_capacity(types.len());
    let mut offset = 0_usize;
    let has_names = !names.is_empty();

    for (index, value_type) in types.iter().copied().enumerate() {
        let logger = if has_names {
            ParamLogger::new(offset, names[index].clone(), value_type.into())
        } else {
            ParamLogger::unnamed(offset, value_type.into())
        };
        loggers.push(logger);
        offset += 1;
        if value_type == ValueType::V128 {
            offset += 1;
        }
    }

    loggers
}

fn build_result_loggers(types: &[ValueType], names: &[String]) -> Vec<ResultLogger> {
    let mut loggers = Vec::with_capacity(types.len());
    let mut offset = 0_usize;
    let has_names = !names.is_empty();

    for (index, value_type) in types.iter().copied().enumerate() {
        let logger = if has_names {
            ResultLogger::new(offset, names[index].clone(), value_type.into())
        } else {
            ResultLogger::unnamed(offset, value_type.into())
        };
        loggers.push(logger);
        offset += 1;
        if value_type == ValueType::V128 {
            offset += 1;
        }
    }

    loggers
}

impl LogValueType {
    pub fn write<W: Write>(
        self,
        memory: Option<&Memory>,
        writer: &mut W,
        index: usize,
        values: &[u64],
    ) -> io::Result<()> {
        match self {
            Self::I32 => write!(writer, "{}", values[index] as u32 as i32),
            Self::I64 => write!(writer, "{}", values[index] as i64),
            Self::F32 => write!(writer, "{}", decode_f32(values[index])),
            Self::F64 => write!(writer, "{}", decode_f64(values[index])),
            Self::V128 => write!(writer, "{:016x}{:016x}", values[index], values[index + 1]),
            Self::FuncRef | Self::ExternRef => write!(writer, "{:016x}", values[index]),
            Self::MemI32 => write_mem_i32(memory, writer, values[index] as u32),
            Self::MemH64 => write_mem_h64(memory, writer, values[index] as u32),
            Self::String => write_string(
                memory,
                writer,
                values[index] as u32,
                values[index + 1] as u32,
            ),
        }
    }
}

pub fn write_mem_i32<W: Write>(
    memory: Option<&Memory>,
    writer: &mut W,
    offset: u32,
) -> io::Result<()> {
    if let Some(value) = memory.and_then(|memory| memory.read_u32_le(offset)) {
        write!(writer, "{}", value as i32)
    } else {
        write_oom(writer, offset, 4)
    }
}

pub fn write_mem_h64<W: Write>(
    memory: Option<&Memory>,
    writer: &mut W,
    offset: u32,
) -> io::Result<()> {
    if let Some(bytes) = memory.and_then(|memory| memory.read(offset as usize, 8)) {
        for byte in bytes {
            write!(writer, "{:02x}", byte)?;
        }
        Ok(())
    } else {
        write_oom(writer, offset, 8)
    }
}

pub fn write_string<W: Write>(
    memory: Option<&Memory>,
    writer: &mut W,
    offset: u32,
    byte_count: u32,
) -> io::Result<()> {
    write_string_or_oom(memory, writer, offset, byte_count)
}

pub fn write_string_or_oom<W: Write>(
    memory: Option<&Memory>,
    writer: &mut W,
    offset: u32,
    byte_count: u32,
) -> io::Result<()> {
    if let Some(bytes) = memory.and_then(|memory| memory.read(offset as usize, byte_count as usize))
    {
        writer.write_all(&bytes)
    } else {
        write_oom(writer, offset, byte_count)
    }
}

pub fn write_oom<W: Write>(writer: &mut W, offset: u32, byte_count: u32) -> io::Result<()> {
    write!(writer, "OOM({offset},{byte_count})")
}

#[cfg(test)]
mod tests {
    use super::{
        config, write_mem_h64, write_mem_i32, write_oom, write_string_or_oom, LogScopes,
        LogValueType,
    };
    use crate::api::wasm::{
        encode_f32, encode_f64, FunctionDefinition, Memory, MemoryDefinition, ValueType,
    };
    use crate::experimental::LinearMemory;

    #[test]
    fn log_scopes_toggle_bits() {
        for scope in [LogScopes::CLOCK, LogScopes::FILESYSTEM] {
            let mut flags = LogScopes::NONE;
            assert!(!flags.is_enabled(scope));
            flags |= scope;
            assert!(flags.is_enabled(scope));
            flags = LogScopes(flags.raw() ^ scope.raw());
            assert!(!flags.is_enabled(scope));
        }
    }

    #[test]
    fn log_scopes_display_matches_go() {
        assert_eq!("", LogScopes::NONE.to_string());
        assert_eq!("all", LogScopes::ALL.to_string());
        assert_eq!("clock", LogScopes::CLOCK.to_string());
        assert_eq!("proc", LogScopes::PROC.to_string());
        assert_eq!("filesystem", LogScopes::FILESYSTEM.to_string());
        assert_eq!("memory", LogScopes::MEMORY.to_string());
        assert_eq!("poll", LogScopes::POLL.to_string());
        assert_eq!("random", LogScopes::RANDOM.to_string());
        assert_eq!("sock", LogScopes::SOCK.to_string());
        assert_eq!(
            "filesystem|random",
            (LogScopes::FILESYSTEM | LogScopes::RANDOM).to_string()
        );
        assert_eq!("<unknown=16384>", LogScopes(1 << 14).to_string());
    }

    #[test]
    fn config_tracks_v128_stack_offsets() {
        let function = FunctionDefinition::new("test")
            .with_signature(
                vec![ValueType::I32, ValueType::V128, ValueType::I64],
                vec![ValueType::V128, ValueType::I32],
            )
            .with_parameter_names(vec!["x".into(), "vec".into(), "y".into()])
            .with_result_names(vec!["r0".into(), "r1".into()]);

        let (params, results) = config(&function);
        let mut written = Vec::new();
        params[0]
            .log(None, &mut written, &[1, 0xAA, 0xBB, 3])
            .expect("param log should succeed");
        assert_eq!(b"x=1", written.as_slice());

        written.clear();
        params[1]
            .log(None, &mut written, &[1, 0x11, 0x22, 3])
            .expect("param log should succeed");
        assert_eq!(b"vec=00000000000000110000000000000022", written.as_slice());

        written.clear();
        params[2]
            .log(None, &mut written, &[1, 0x11, 0x22, 3])
            .expect("param log should succeed");
        assert_eq!(b"y=3", written.as_slice());

        written.clear();
        results[0]
            .log(None, &mut written, &[0x33, 0x44, 5])
            .expect("result log should succeed");
        assert_eq!(b"r0=00000000000000330000000000000044", written.as_slice());

        written.clear();
        results[1]
            .log(None, &mut written, &[0x33, 0x44, 5])
            .expect("result log should succeed");
        assert_eq!(b"r1=5", written.as_slice());
    }

    #[test]
    fn value_writers_match_go_formatting() {
        let mut written = Vec::new();
        LogValueType::I32
            .write(None, &mut written, 0, &[u32::MAX as u64])
            .expect("i32 write should succeed");
        assert_eq!(b"-1", written.as_slice());

        written.clear();
        LogValueType::I64
            .write(None, &mut written, 0, &[u64::MAX])
            .expect("i64 write should succeed");
        assert_eq!(b"-1", written.as_slice());

        written.clear();
        LogValueType::F32
            .write(None, &mut written, 0, &[encode_f32(1.5)])
            .expect("f32 write should succeed");
        assert_eq!(b"1.5", written.as_slice());

        written.clear();
        LogValueType::F64
            .write(None, &mut written, 0, &[encode_f64(2.5)])
            .expect("f64 write should succeed");
        assert_eq!(b"2.5", written.as_slice());

        written.clear();
        LogValueType::ExternRef
            .write(None, &mut written, 0, &[0x2A])
            .expect("ref write should succeed");
        assert_eq!(b"000000000000002a", written.as_slice());
    }

    #[test]
    fn memory_writers_and_oom_match_go() {
        let memory = memory_with_bytes(&[
            0x78, 0x56, 0x34, 0x12, b'h', b'e', b'l', b'l', b'o', 1, 2, 3, 4, 5, 6, 7, 8,
        ]);
        let mut written = Vec::new();

        write_mem_i32(Some(&memory), &mut written, 0).expect("mem i32 write should succeed");
        assert_eq!(b"305419896", written.as_slice());

        written.clear();
        write_mem_h64(Some(&memory), &mut written, 9).expect("mem h64 write should succeed");
        assert_eq!(b"0102030405060708", written.as_slice());

        written.clear();
        write_string_or_oom(Some(&memory), &mut written, 4, 5)
            .expect("string write should succeed");
        assert_eq!(b"hello", written.as_slice());

        written.clear();
        write_string_or_oom(Some(&memory), &mut written, 62, 4).expect("oom write should succeed");
        assert_eq!(b"OOM(62,4)", written.as_slice());

        written.clear();
        write_oom(&mut written, 1, 2).expect("oom write should succeed");
        assert_eq!(b"OOM(1,2)", written.as_slice());
    }

    fn memory_with_bytes(bytes: &[u8]) -> Memory {
        let len = bytes.len().max(64);
        let mut linear = LinearMemory::new(len, len);
        linear.bytes_mut()[..bytes.len()].copy_from_slice(bytes);
        Memory::new(MemoryDefinition::new(0, None), linear)
    }
}
