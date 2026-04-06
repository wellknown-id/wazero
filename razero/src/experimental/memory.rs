use std::sync::Arc;

use crate::ctx_keys::Context;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LinearMemory {
    bytes: Vec<u8>,
    max: usize,
}

impl LinearMemory {
    pub fn new(initial: usize, max: usize) -> Self {
        Self {
            bytes: vec![0; initial],
            max,
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn reallocate(&mut self, size: usize) -> Option<&mut [u8]> {
        if size > self.max {
            return None;
        }
        self.bytes.resize(size, 0);
        Some(&mut self.bytes)
    }

    pub fn free(&mut self) {
        self.bytes.clear();
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

pub fn with_memory_allocator(
    ctx: &Context,
    allocator: impl MemoryAllocator + 'static,
) -> Context {
    let mut cloned = ctx.clone();
    cloned.memory_allocator = Some(Arc::new(allocator));
    cloned
}

pub fn get_memory_allocator(ctx: &Context) -> Option<Arc<dyn MemoryAllocator>> {
    ctx.memory_allocator.clone()
}
