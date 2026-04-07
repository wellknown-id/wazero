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
                if size < previous {
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

impl<F> MemoryAllocator for F
where
    F: Fn(usize, usize) -> Option<LinearMemory> + Send + Sync,
{
    fn allocate(&self, cap: usize, max: usize) -> Option<LinearMemory> {
        (self)(cap, max)
    }
}

pub struct MemoryAllocatorFn<F>(F);

impl<F> MemoryAllocatorFn<F> {
    pub fn new(callback: F) -> Self {
        Self(callback)
    }
}

impl<F> MemoryAllocator for MemoryAllocatorFn<F>
where
    F: Fn(usize, usize) -> Option<LinearMemory> + Send + Sync,
{
    fn allocate(&self, cap: usize, max: usize) -> Option<LinearMemory> {
        (self.0)(cap, max)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultMemoryAllocator;

impl MemoryAllocator for DefaultMemoryAllocator {
    fn allocate(&self, cap: usize, max: usize) -> Option<LinearMemory> {
        Some(LinearMemory::new(cap, max))
    }
}

pub trait IntoMemoryAllocator {
    fn into_memory_allocator(self) -> Option<Arc<dyn MemoryAllocator>>;
}

impl<T> IntoMemoryAllocator for T
where
    T: MemoryAllocator + 'static,
{
    fn into_memory_allocator(self) -> Option<Arc<dyn MemoryAllocator>> {
        Some(Arc::new(self))
    }
}

impl<T> IntoMemoryAllocator for Option<T>
where
    T: MemoryAllocator + 'static,
{
    fn into_memory_allocator(self) -> Option<Arc<dyn MemoryAllocator>> {
        self.map(|allocator| Arc::new(allocator) as Arc<dyn MemoryAllocator>)
    }
}

pub fn with_memory_allocator(ctx: &Context, allocator: impl IntoMemoryAllocator) -> Context {
    let Some(allocator) = allocator.into_memory_allocator() else {
        return ctx.clone();
    };
    let mut cloned = ctx.clone();
    cloned.memory_allocator = Some(allocator);
    cloned
}

pub fn get_memory_allocator(ctx: &Context) -> Option<Arc<dyn MemoryAllocator>> {
    ctx.memory_allocator.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::{
        get_memory_allocator, with_memory_allocator, DefaultMemoryAllocator, LinearMemory,
        MemoryAllocator, MemoryAllocatorFn,
    };
    use crate::Context;

    #[test]
    fn memory_allocator_fn_delegates() {
        let calls = Arc::new(AtomicUsize::new(0));
        let allocator = MemoryAllocatorFn::new({
            let calls = calls.clone();
            move |cap, max| {
                calls.fetch_add(1, Ordering::SeqCst);
                Some(LinearMemory::new(cap, max))
            }
        });

        let memory = allocator
            .allocate(4, 8)
            .expect("memory should be allocated");
        assert_eq!(4, memory.len());
        assert_eq!(1, calls.load(Ordering::SeqCst));
    }

    #[test]
    fn memory_allocator_round_trips_through_context() {
        let ctx = with_memory_allocator(
            &Context::default(),
            MemoryAllocatorFn::new(|cap, max| Some(LinearMemory::new(cap, max))),
        );
        let allocator = get_memory_allocator(&ctx).expect("allocator should be present");
        let memory = allocator
            .allocate(2, 3)
            .expect("memory should be allocated");
        assert_eq!(2, memory.len());
    }

    #[test]
    fn with_memory_allocator_accepts_closure() {
        let ctx = with_memory_allocator(&Context::default(), |cap, max| {
            Some(LinearMemory::new(cap + 1, max + 1))
        });

        let memory = get_memory_allocator(&ctx)
            .expect("allocator should be present")
            .allocate(2, 3)
            .expect("memory should be allocated");
        assert_eq!(3, memory.len());
    }

    #[test]
    fn with_memory_allocator_none_is_noop() {
        let mut ctx = Context::default();
        ctx.insert(crate::ctx_keys::ContextKey::custom("marker"), "ok");

        let updated = with_memory_allocator(&ctx, Option::<DefaultMemoryAllocator>::None);

        assert!(get_memory_allocator(&updated).is_none());
        assert_eq!(
            Some("ok"),
            updated.get(&crate::ctx_keys::ContextKey::custom("marker"))
        );
    }
}
