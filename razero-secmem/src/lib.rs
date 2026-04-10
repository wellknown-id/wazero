#![doc = "Guard-page backed secure-memory helpers."]

use std::fmt::{Display, Formatter};

use razero_platform::{GuardPageError, LinearMemory};

pub type SecMemResult<T> = Result<T, SecMemError>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuardPageAllocator;

impl GuardPageAllocator {
    pub fn allocate_zeroed(&self, len: usize) -> SecMemResult<GuardedAllocation> {
        GuardedAllocation::new(len)
    }
}

pub struct GuardedAllocation {
    memory: Option<LinearMemory>,
    len: usize,
}

impl GuardedAllocation {
    pub fn new(len: usize) -> SecMemResult<Self> {
        Self::with_reservation(len, len)
    }

    pub fn with_reservation(committed_len: usize, reserved_len: usize) -> SecMemResult<Self> {
        if committed_len == 0 && reserved_len == 0 {
            return Ok(Self {
                memory: None,
                len: 0,
            });
        }

        let memory =
            LinearMemory::reserve(reserved_len, committed_len).map_err(SecMemError::Platform)?;
        Ok(Self {
            memory: Some(memory),
            len: reserved_len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn committed_len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn as_slice(&self) -> &[u8] {
        match &self.memory {
            Some(memory) => memory.committed_slice(),
            None => &[],
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match &mut self.memory {
            Some(memory) => memory.committed_slice_mut(),
            None => &mut [],
        }
    }

    pub fn grow(&mut self, new_len: usize) -> SecMemResult<()> {
        match &mut self.memory {
            Some(memory) => memory.grow(new_len).map_err(SecMemError::Platform),
            None if new_len == 0 => Ok(()),
            None => {
                *self = Self::with_reservation(new_len, new_len)?;
                Ok(())
            }
        }
    }

    fn clear(&mut self) {
        if let Some(memory) = &mut self.memory {
            memory.committed_slice_mut().fill(0);
            let _ = memory.unmap();
        }
        self.memory = None;
        self.len = 0;
    }
}

impl Clone for GuardedAllocation {
    fn clone(&self) -> Self {
        let mut cloned = Self::with_reservation(self.committed_len(), self.len)
            .expect("guarded allocation clone should succeed");
        if self.committed_len() != 0 {
            cloned.as_mut_slice().copy_from_slice(self.as_slice());
        }
        cloned
    }
}

impl std::fmt::Debug for GuardedAllocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedAllocation")
            .field("reserved_len", &self.len)
            .field("committed_len", &self.committed_len())
            .finish()
    }
}

impl PartialEq for GuardedAllocation {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for GuardedAllocation {}

impl Default for GuardedAllocation {
    fn default() -> Self {
        Self {
            memory: None,
            len: 0,
        }
    }
}

impl Drop for GuardedAllocation {
    fn drop(&mut self) {
        self.clear();
    }
}

unsafe impl Send for GuardedAllocation {}
unsafe impl Sync for GuardedAllocation {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecMemError {
    Platform(GuardPageError),
}

impl Display for SecMemError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Platform(error) => Display::fmt(error, f),
        }
    }
}

impl std::error::Error for SecMemError {}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use super::SecMemError;
    use super::{GuardPageAllocator, GuardedAllocation};
    #[cfg(not(target_os = "linux"))]
    use razero_platform::GuardPageError;

    #[test]
    fn allocation_is_zeroed() {
        let allocation = GuardPageAllocator
            .allocate_zeroed(64)
            .expect("allocation should succeed");
        assert_eq!(64, allocation.len());
        assert_eq!(64, allocation.committed_len());
        assert!(allocation.as_slice().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn allocation_clone_copies_bytes() {
        let mut allocation = GuardedAllocation::new(8).expect("allocation should succeed");
        allocation
            .as_mut_slice()
            .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let clone = allocation.clone();
        assert_eq!(allocation, clone);
    }

    #[test]
    fn empty_allocation_is_supported() {
        let allocation = GuardPageAllocator
            .allocate_zeroed(0)
            .expect("allocation should succeed");
        assert!(allocation.is_empty());
        assert!(allocation.as_slice().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allocation_can_reserve_more_than_it_commits_and_grow() {
        let mut allocation = GuardedAllocation::with_reservation(64 << 10, 128 << 10)
            .expect("allocation should succeed");
        assert_eq!(128 << 10, allocation.len());
        assert_eq!(64 << 10, allocation.committed_len());
        allocation.as_mut_slice().fill(0xaa);

        allocation.grow(96 << 10).expect("grow should succeed");
        assert_eq!(96 << 10, allocation.committed_len());
        assert!(allocation.as_slice()[(64 << 10)..(96 << 10)]
            .iter()
            .all(|byte| *byte == 0));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn allocation_reports_unsupported_on_unsupported_targets() {
        let err = GuardPageAllocator
            .allocate_zeroed(64)
            .expect_err("unsupported targets should reject guard-page allocation");

        assert!(matches!(
            err,
            SecMemError::Platform(GuardPageError::Unsupported(_))
        ));
    }
}
