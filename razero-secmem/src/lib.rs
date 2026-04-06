#![doc = "Minimal secure-memory scaffolding."]

pub type SecMemResult<T> = Result<T, SecMemError>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardPageAllocator;

impl GuardPageAllocator {
    pub fn allocate_zeroed(&self, len: usize) -> SecMemResult<GuardedAllocation> {
        Ok(GuardedAllocation {
            bytes: vec![0; len],
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardedAllocation {
    bytes: Vec<u8>,
}

impl GuardedAllocation {
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecMemError;
