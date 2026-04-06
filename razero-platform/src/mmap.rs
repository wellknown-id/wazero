use std::error::Error;
use std::fmt::{Display, Formatter};
use std::ptr::NonNull;
use std::slice;

#[cfg(target_os = "linux")]
use crate::mmap_linux as imp;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
use crate::mmap_other as imp;
#[cfg(target_os = "windows")]
use crate::mmap_windows as imp;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformKind {
    Linux,
    Windows,
    Other,
}

pub struct CodeSegment {
    base: Option<NonNull<u8>>,
    len: usize,
    executable: bool,
    platform: PlatformKind,
}

impl CodeSegment {
    pub(crate) unsafe fn from_raw_parts(base: *mut u8, len: usize, platform: PlatformKind) -> Self {
        Self {
            base: NonNull::new(base),
            len,
            executable: false,
            platform,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        match self.base {
            Some(base) => unsafe { slice::from_raw_parts(base.as_ptr().cast_const(), self.len) },
            None => &[],
        }
    }

    pub fn as_mut_slice(&mut self) -> Result<&mut [u8], MmapError> {
        if self.executable {
            return Err(MmapError::InvalidState(
                "code segment is executable and no longer writable",
            ));
        }

        match self.base {
            Some(base) => unsafe { Ok(slice::from_raw_parts_mut(base.as_ptr(), self.len)) },
            None => Ok(&mut []),
        }
    }

    pub fn is_executable(&self) -> bool {
        self.executable
    }

    pub fn as_ptr(&self) -> *const u8 {
        match self.base {
            Some(base) => base.as_ptr().cast_const(),
            None => std::ptr::null(),
        }
    }

    pub(crate) fn base(&self) -> Result<NonNull<u8>, MmapError> {
        self.base
            .ok_or(MmapError::InvalidState("code segment is not mapped"))
    }

    pub(crate) fn set_executable(&mut self, executable: bool) {
        self.executable = executable;
    }

    pub(crate) fn clear(&mut self) {
        self.base = None;
        self.len = 0;
        self.executable = false;
    }
}

impl std::fmt::Debug for CodeSegment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeSegment")
            .field("base", &self.base)
            .field("len", &self.len)
            .field("executable", &self.executable)
            .field("platform", &self.platform)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MmapError {
    ZeroLength,
    InvalidState(&'static str),
    Syscall { operation: &'static str, errno: i32 },
    Unsupported(&'static str),
}

impl Display for MmapError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroLength => f.write_str("code segment length must be non-zero"),
            Self::InvalidState(message) => f.write_str(message),
            Self::Syscall { operation, errno } => {
                write!(f, "{operation} failed with errno {errno}")
            }
            Self::Unsupported(message) => f.write_str(message),
        }
    }
}

impl Error for MmapError {}

pub fn map_code_segment(len: usize) -> Result<CodeSegment, MmapError> {
    imp::map_code_segment_impl(len)
}

pub fn protect_code_segment(segment: &mut CodeSegment) -> Result<(), MmapError> {
    imp::protect_code_segment_impl(segment)
}

pub fn unmap_code_segment(segment: &mut CodeSegment) -> Result<(), MmapError> {
    imp::unmap_code_segment_impl(segment)
}

#[cfg(test)]
mod tests {
    use super::{map_code_segment, protect_code_segment, unmap_code_segment};

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_code_segment_lifecycle() {
        let mut segment = map_code_segment(4096).expect("map should succeed");
        let bytes = segment
            .as_mut_slice()
            .expect("fresh mapping should be writable");
        bytes[0] = 0xAA;
        bytes[4095] = 0xBB;

        protect_code_segment(&mut segment).expect("protect should succeed");
        assert!(segment.is_executable());
        assert!(segment.as_mut_slice().is_err());
        assert_eq!(segment.as_slice()[0], 0xAA);
        assert_eq!(segment.as_slice()[4095], 0xBB);

        unmap_code_segment(&mut segment).expect("unmap should succeed");
        assert!(segment.is_empty());
        assert!(unmap_code_segment(&mut segment).is_err());
    }
}
