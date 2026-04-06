use std::error::Error;
use std::fmt::{Display, Formatter};
use std::ptr::NonNull;
use std::slice;

pub const GUARD_REGION_SIZE: usize = 4 << 30;

const UNSUPPORTED: &str = "guard-page linear memory is only implemented on Linux targets";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuardPageSupport {
    pub supported: bool,
    pub guard_region_size: usize,
    pub reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinearMemoryLayout {
    pub reserved_bytes: usize,
    pub committed_bytes: usize,
    pub guard_bytes: usize,
}

impl LinearMemoryLayout {
    pub const fn total_address_space_bytes(self) -> usize {
        self.reserved_bytes + self.guard_bytes
    }
}

pub struct LinearMemory {
    base: Option<NonNull<u8>>,
    layout: LinearMemoryLayout,
}

impl LinearMemory {
    pub fn reserve(reserve_bytes: usize, commit_bytes: usize) -> Result<Self, GuardPageError> {
        if commit_bytes > reserve_bytes {
            return Err(GuardPageError::InvalidArguments(
                "committed bytes cannot exceed reserved bytes",
            ));
        }

        reserve_linear_memory(reserve_bytes, commit_bytes)
    }

    pub fn layout(&self) -> LinearMemoryLayout {
        self.layout
    }

    pub fn base_ptr(&self) -> *mut u8 {
        self.base.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    pub fn committed_slice(&self) -> &[u8] {
        match self.base {
            Some(base) if self.layout.committed_bytes != 0 => unsafe {
                slice::from_raw_parts(base.as_ptr().cast_const(), self.layout.committed_bytes)
            },
            _ => &[],
        }
    }

    pub fn committed_slice_mut(&mut self) -> &mut [u8] {
        match self.base {
            Some(base) if self.layout.committed_bytes != 0 => unsafe {
                slice::from_raw_parts_mut(base.as_ptr(), self.layout.committed_bytes)
            },
            _ => &mut [],
        }
    }

    pub fn grow(&mut self, new_commit_bytes: usize) -> Result<(), GuardPageError> {
        grow_linear_memory(self, new_commit_bytes)
    }

    pub fn unmap(&mut self) -> Result<(), GuardPageError> {
        unmap_linear_memory(self)
    }

    pub(crate) unsafe fn from_raw_parts(base: *mut u8, layout: LinearMemoryLayout) -> Self {
        Self {
            base: NonNull::new(base),
            layout,
        }
    }

    pub(crate) fn base(&self) -> Result<NonNull<u8>, GuardPageError> {
        self.base
            .ok_or(GuardPageError::InvalidState("linear memory is not mapped"))
    }

    pub(crate) fn set_committed_bytes(&mut self, committed_bytes: usize) {
        self.layout.committed_bytes = committed_bytes;
    }

    pub(crate) fn clear(&mut self) {
        self.base = None;
        self.layout = LinearMemoryLayout {
            reserved_bytes: 0,
            committed_bytes: 0,
            guard_bytes: 0,
        };
    }
}

impl std::fmt::Debug for LinearMemory {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearMemory")
            .field("base", &self.base)
            .field("layout", &self.layout)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardPageError {
    InvalidArguments(&'static str),
    InvalidState(&'static str),
    SizeOverflow,
    Syscall { operation: &'static str, errno: i32 },
    Unsupported(&'static str),
}

impl Display for GuardPageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArguments(message)
            | Self::InvalidState(message)
            | Self::Unsupported(message) => f.write_str(message),
            Self::SizeOverflow => f.write_str("linear memory reservation size overflowed usize"),
            Self::Syscall { operation, errno } => {
                write!(f, "{operation} failed with errno {errno}")
            }
        }
    }
}

impl Error for GuardPageError {}

pub fn supports_guard_pages() -> bool {
    cfg!(target_os = "linux")
}

pub fn guard_page_support() -> GuardPageSupport {
    if supports_guard_pages() {
        GuardPageSupport {
            supported: true,
            guard_region_size: GUARD_REGION_SIZE,
            reason: None,
        }
    } else {
        GuardPageSupport {
            supported: false,
            guard_region_size: GUARD_REGION_SIZE,
            reason: Some(UNSUPPORTED),
        }
    }
}

#[cfg(target_os = "linux")]
fn reserve_linear_memory(
    reserve_bytes: usize,
    commit_bytes: usize,
) -> Result<LinearMemory, GuardPageError> {
    let total = reserve_bytes
        .checked_add(GUARD_REGION_SIZE)
        .ok_or(GuardPageError::SizeOverflow)?;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            total,
            libc::PROT_NONE,
            libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(errno_error("mmap"));
    }

    let memory = unsafe {
        LinearMemory::from_raw_parts(
            ptr.cast(),
            LinearMemoryLayout {
                reserved_bytes: reserve_bytes,
                committed_bytes: commit_bytes,
                guard_bytes: GUARD_REGION_SIZE,
            },
        )
    };

    if commit_bytes != 0 {
        let rc = unsafe {
            libc::mprotect(
                memory.base_ptr().cast(),
                commit_bytes,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if rc != 0 {
            let _ = unsafe { libc::munmap(memory.base_ptr().cast(), total) };
            return Err(errno_error("mprotect"));
        }
    }

    Ok(memory)
}

#[cfg(not(target_os = "linux"))]
fn reserve_linear_memory(
    _reserve_bytes: usize,
    _commit_bytes: usize,
) -> Result<LinearMemory, GuardPageError> {
    Err(GuardPageError::Unsupported(UNSUPPORTED))
}

#[cfg(target_os = "linux")]
fn grow_linear_memory(
    memory: &mut LinearMemory,
    new_commit_bytes: usize,
) -> Result<(), GuardPageError> {
    if new_commit_bytes <= memory.layout.committed_bytes {
        return Ok(());
    }
    if new_commit_bytes > memory.layout.reserved_bytes {
        return Err(GuardPageError::InvalidArguments(
            "committed bytes cannot exceed reserved bytes",
        ));
    }

    let base = memory.base()?;
    let rc = unsafe {
        libc::mprotect(
            base.as_ptr().add(memory.layout.committed_bytes).cast(),
            new_commit_bytes - memory.layout.committed_bytes,
            libc::PROT_READ | libc::PROT_WRITE,
        )
    };
    if rc != 0 {
        return Err(errno_error("mprotect"));
    }

    memory.set_committed_bytes(new_commit_bytes);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn grow_linear_memory(
    _memory: &mut LinearMemory,
    _new_commit_bytes: usize,
) -> Result<(), GuardPageError> {
    Err(GuardPageError::Unsupported(UNSUPPORTED))
}

#[cfg(target_os = "linux")]
fn unmap_linear_memory(memory: &mut LinearMemory) -> Result<(), GuardPageError> {
    let base = memory.base()?;
    let rc = unsafe {
        libc::munmap(
            base.as_ptr().cast(),
            memory.layout.total_address_space_bytes(),
        )
    };
    if rc != 0 {
        return Err(errno_error("munmap"));
    }
    memory.clear();
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn unmap_linear_memory(_memory: &mut LinearMemory) -> Result<(), GuardPageError> {
    Err(GuardPageError::Unsupported(UNSUPPORTED))
}

fn errno_error(operation: &'static str) -> GuardPageError {
    GuardPageError::Syscall {
        operation,
        errno: std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        guard_page_support, supports_guard_pages, GuardPageError, LinearMemory, GUARD_REGION_SIZE,
    };

    #[cfg(target_os = "linux")]
    #[test]
    fn linear_memory_reserve_grow_and_unmap() {
        let mut memory = LinearMemory::reserve(64 << 10, 16 << 10).expect("reserve should succeed");
        assert_eq!(memory.layout().reserved_bytes, 64 << 10);
        assert_eq!(memory.layout().guard_bytes, GUARD_REGION_SIZE);
        assert_eq!(memory.committed_slice().len(), 16 << 10);

        let slice = memory.committed_slice_mut();
        slice[0] = 0x11;
        slice[(16 << 10) - 1] = 0x22;

        memory.grow(32 << 10).expect("grow should succeed");
        assert_eq!(memory.committed_slice().len(), 32 << 10);
        assert_eq!(memory.committed_slice()[0], 0x11);
        assert_eq!(memory.committed_slice()[(16 << 10) - 1], 0x22);

        memory.unmap().expect("unmap should succeed");
        assert_eq!(memory.committed_slice().len(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn grow_past_reservation_is_rejected() {
        let mut memory = LinearMemory::reserve(32 << 10, 0).expect("reserve should succeed");
        let error = memory.grow(64 << 10).expect_err("grow should fail");
        assert_eq!(
            error,
            GuardPageError::InvalidArguments("committed bytes cannot exceed reserved bytes")
        );
        memory.unmap().expect("unmap should succeed");
    }

    #[test]
    fn support_query_matches_linux_implementation_boundary() {
        let support = guard_page_support();
        assert_eq!(support.supported, supports_guard_pages());
        if cfg!(target_os = "linux") {
            assert!(support.reason.is_none());
        } else {
            assert!(support.reason.is_some());
        }
    }
}
