use std::sync::Arc;

use razero_secmem::GuardedAllocation;

use crate::ctx_keys::Context;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinearMemory {
    backing: LinearMemoryBacking,
    len: usize,
    max: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LinearMemoryBacking {
    Vec(Vec<u8>),
    Guarded(GuardedAllocation),
}

impl Default for LinearMemoryBacking {
    fn default() -> Self {
        Self::Vec(Vec::new())
    }
}

impl Default for LinearMemory {
    fn default() -> Self {
        Self {
            backing: LinearMemoryBacking::default(),
            len: 0,
            max: 0,
        }
    }
}

impl LinearMemory {
    pub fn new(initial: usize, max: usize) -> Self {
        Self {
            backing: LinearMemoryBacking::Vec(vec![0; initial]),
            len: initial,
            max,
        }
    }

    pub fn from_guarded(allocation: GuardedAllocation, initial: usize, max: usize) -> Self {
        Self {
            backing: LinearMemoryBacking::Guarded(allocation),
            len: initial,
            max,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> &[u8] {
        match &self.backing {
            LinearMemoryBacking::Vec(bytes) => &bytes[..self.len],
            LinearMemoryBacking::Guarded(allocation) => &allocation.as_slice()[..self.len],
        }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        match &mut self.backing {
            LinearMemoryBacking::Vec(bytes) => &mut bytes[..self.len],
            LinearMemoryBacking::Guarded(allocation) => &mut allocation.as_mut_slice()[..self.len],
        }
    }

    pub fn reallocate(&mut self, size: usize) -> Option<&mut [u8]> {
        if size > self.max {
            return None;
        }
        match &mut self.backing {
            LinearMemoryBacking::Vec(bytes) => {
                bytes.resize(size, 0);
                self.len = size;
                Some(&mut bytes[..self.len])
            }
            LinearMemoryBacking::Guarded(allocation) => {
                if size > allocation.len() {
                    return None;
                }
                let previous = self.len;
                if size < allocation.len() {
                    allocation.as_mut_slice()[size..previous].fill(0);
                }
                self.len = size;
                Some(&mut allocation.as_mut_slice()[..self.len])
            }
        }
    }

    pub fn free(&mut self) {
        match &mut self.backing {
            LinearMemoryBacking::Vec(bytes) => bytes.clear(),
            LinearMemoryBacking::Guarded(allocation) => allocation.as_mut_slice().fill(0),
        }
        self.len = 0;
    }
}

pub trait MemoryAllocator: Send + Sync {
    fn allocate(&self, cap: usize, max: usize) -> Option<LinearMemory>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultMemoryAllocator;

impl MemoryAllocator for DefaultMemoryAllocator {
    fn allocate(&self, cap: usize, max: usize) -> Option<LinearMemory> {
        Some(LinearMemory::new(cap, max))
    }
}

pub fn with_memory_allocator(ctx: &Context, allocator: impl MemoryAllocator + 'static) -> Context {
    let mut cloned = ctx.clone();
    cloned.memory_allocator = Some(Arc::new(allocator));
    cloned
}

pub fn get_memory_allocator(ctx: &Context) -> Option<Arc<dyn MemoryAllocator>> {
    ctx.memory_allocator.clone()
}
