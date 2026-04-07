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
        if len == 0 {
            return Ok(Self {
                memory: None,
                len: 0,
            });
        }

        let memory = LinearMemory::reserve(len, len).map_err(SecMemError::Platform)?;
        Ok(Self {
            memory: Some(memory),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
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
        let mut cloned = Self::new(self.len).expect("guarded allocation clone should succeed");
        if self.len != 0 {
            cloned.as_mut_slice().copy_from_slice(self.as_slice());
        }
        cloned
    }
}

impl std::fmt::Debug for GuardedAllocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedAllocation")
            .field("len", &self.len)
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
    use super::{GuardPageAllocator, GuardedAllocation};

    #[test]
    fn allocation_is_zeroed() {
        let allocation = GuardPageAllocator
            .allocate_zeroed(64)
            .expect("allocation should succeed");
        assert_eq!(64, allocation.len());
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
}
