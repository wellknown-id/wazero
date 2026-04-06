use std::{
    borrow::Cow,
    error::Error,
    fmt::{self, Display, Formatter},
};

use crate::experimental::r#yield::YieldError;

pub type Result<T> = std::result::Result<T, RuntimeError>;

pub const EXIT_CODE_CONTEXT_CANCELED: u32 = 0xffff_ffff;
pub const EXIT_CODE_DEADLINE_EXCEEDED: u32 = 0xefff_ffff;

#[derive(Debug)]
pub enum RuntimeError {
    Message(Cow<'static, str>),
    Exit(ExitError),
    Yield(YieldError),
}

impl RuntimeError {
    pub fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Message(message.into())
    }

    pub fn message(&self) -> String {
        match self {
            Self::Message(message) => message.to_string(),
            Self::Exit(error) => error.to_string(),
            Self::Yield(error) => error.to_string(),
        }
    }

    pub fn exit_code(&self) -> Option<u32> {
        match self {
            Self::Exit(error) => Some(error.exit_code()),
            _ => None,
        }
    }
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Exit(error) => Display::fmt(error, f),
            Self::Yield(error) => Display::fmt(error, f),
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Exit(error) => Some(error),
            Self::Yield(error) => Some(error),
            Self::Message(_) => None,
        }
    }
}

impl From<ExitError> for RuntimeError {
    fn from(value: ExitError) -> Self {
        Self::Exit(value)
    }
}

impl From<YieldError> for RuntimeError {
    fn from(value: YieldError) -> Self {
        Self::Yield(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExitError {
    exit_code: u32,
}

impl ExitError {
    pub fn new(exit_code: u32) -> Self {
        Self { exit_code }
    }

    pub fn exit_code(&self) -> u32 {
        self.exit_code
    }
}

impl Display for ExitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.exit_code {
            EXIT_CODE_CONTEXT_CANCELED => f.write_str("module closed with context canceled"),
            EXIT_CODE_DEADLINE_EXCEEDED => f.write_str("module closed with deadline exceeded"),
            exit_code => write!(f, "module closed with exit_code({exit_code})"),
        }
    }
}

impl Error for ExitError {}
