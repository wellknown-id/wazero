//! Resettable pooling utilities used across compiler phases.

use core::mem::MaybeUninit;
use core::ptr::NonNull;
use core::slice;

const POOL_PAGE_SIZE: usize = 128;
const ARRAY_SIZE: usize = 8;

pub struct Pool<T: Default> {
    pages: Vec<Box<[T; POOL_PAGE_SIZE]>>,
    reset_fn: Option<fn(&mut T)>,
    active_pages: usize,
    allocated: usize,
    index: usize,
}

impl<T: Default> Pool<T> {
    pub fn new(reset_fn: Option<fn(&mut T)>) -> Self {
        let mut pool = Self {
            pages: Vec::new(),
            reset_fn,
            active_pages: 0,
            allocated: 0,
            index: POOL_PAGE_SIZE,
        };
        pool.reset();
        pool
    }

    pub fn allocated(&self) -> usize {
        self.allocated
    }

    pub fn allocate(&mut self) -> &mut T {
        if self.index == POOL_PAGE_SIZE {
            if self.active_pages == self.pages.len() {
                self.pages
                    .push(Box::new(std::array::from_fn(|_| T::default())));
            }
            self.active_pages += 1;
            self.index = 0;
        }

        let page_index = self.active_pages - 1;
        let item_index = self.index;
        let item = &mut self.pages[page_index][item_index];
        if let Some(reset_fn) = self.reset_fn {
            reset_fn(item);
        }
        self.index += 1;
        self.allocated += 1;
        item
    }

    pub fn view(&self, index: usize) -> &T {
        let (page, offset) = (index / POOL_PAGE_SIZE, index % POOL_PAGE_SIZE);
        &self.pages[page][offset]
    }

    pub fn view_mut(&mut self, index: usize) -> &mut T {
        let (page, offset) = (index / POOL_PAGE_SIZE, index % POOL_PAGE_SIZE);
        &mut self.pages[page][offset]
    }

    pub fn reset(&mut self) {
        self.active_pages = 0;
        self.index = POOL_PAGE_SIZE;
        self.allocated = 0;
    }
}

pub struct IDedPool<T: Default> {
    pool: Pool<T>,
    id_to_items: Vec<Option<usize>>,
    max_id_encountered: isize,
}

impl<T: Default> IDedPool<T> {
    pub fn new(reset_fn: Option<fn(&mut T)>) -> Self {
        Self {
            pool: Pool::new(reset_fn),
            id_to_items: Vec::new(),
            max_id_encountered: -1,
        }
    }

    pub fn get_or_allocate(&mut self, id: usize) -> &mut T {
        if self.max_id_encountered < id as isize {
            self.max_id_encountered = id as isize;
        }

        if id >= self.id_to_items.len() {
            self.id_to_items.resize(id + 1, None);
        }

        if let Some(index) = self.id_to_items[id] {
            return self.pool.view_mut(index);
        }

        let index = self.pool.allocated();
        self.id_to_items[id] = Some(index);
        self.pool.allocate()
    }

    pub fn get(&self, id: usize) -> Option<&T> {
        self.id_to_items
            .get(id)
            .and_then(|entry| entry.map(|index| self.pool.view(index)))
    }

    pub fn reset(&mut self) {
        self.pool.reset();
        if self.max_id_encountered >= 0 {
            for index in 0..=self.max_id_encountered as usize {
                if let Some(entry) = self.id_to_items.get_mut(index) {
                    *entry = None;
                }
            }
        }
        self.max_id_encountered = -1;
    }

    pub fn max_id_encountered(&self) -> isize {
        self.max_id_encountered
    }
}

pub struct VarLengthPool<T> {
    array_pool: Pool<VarLengthPoolArray<T>>,
    slice_pool: Pool<Vec<T>>,
}

struct VarLengthPoolArray<T> {
    arr: [MaybeUninit<T>; ARRAY_SIZE],
    next: usize,
}

impl<T> Default for VarLengthPoolArray<T> {
    fn default() -> Self {
        Self {
            arr: std::array::from_fn(|_| MaybeUninit::uninit()),
            next: 0,
        }
    }
}

impl<T> VarLengthPoolArray<T> {
    fn clear(&mut self) {
        for index in 0..self.next {
            unsafe { self.arr[index].assume_init_drop() };
        }
        self.next = 0;
    }
}

impl<T> Drop for VarLengthPoolArray<T> {
    fn drop(&mut self) {
        self.clear();
    }
}

pub struct VarLength<T> {
    arr: Option<NonNull<VarLengthPoolArray<T>>>,
    slc: Option<NonNull<Vec<T>>>,
}

impl<T> VarLengthPool<T> {
    pub fn new() -> Self {
        Self {
            array_pool: Pool::new(Some(|v: &mut VarLengthPoolArray<T>| v.clear())),
            slice_pool: Pool::new(Some(|v: &mut Vec<T>| v.clear())),
        }
    }

    pub fn allocate(&mut self, known_min: usize) -> VarLength<T> {
        if known_min <= ARRAY_SIZE {
            let arr = NonNull::from(self.array_pool.allocate());
            VarLength {
                arr: Some(arr),
                slc: None,
            }
        } else {
            let slc = self.slice_pool.allocate();
            if slc.capacity() < known_min {
                slc.reserve(known_min - slc.capacity());
            }
            VarLength {
                arr: None,
                slc: Some(NonNull::from(slc)),
            }
        }
    }

    pub fn reset(&mut self) {
        self.array_pool.reset();
        self.slice_pool.reset();
    }
}

impl<T> Default for VarLengthPool<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> VarLength<T> {
    pub fn new_nil() -> Self {
        Self {
            arr: None,
            slc: None,
        }
    }

    pub fn append<I>(mut self, pool: &mut VarLengthPool<T>, items: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        if let Some(mut slc) = self.slc {
            unsafe { slc.as_mut().extend(items) };
            return self;
        }

        if self.arr.is_none() {
            self.arr = Some(NonNull::from(pool.array_pool.allocate()));
        }

        let mut iter = items.into_iter().peekable();
        while let Some(item) = iter.next() {
            let arr = unsafe { self.arr.unwrap().as_mut() };
            if arr.next < ARRAY_SIZE {
                arr.arr[arr.next].write(item);
                arr.next += 1;
                continue;
            }

            let slc = pool.slice_pool.allocate();
            let lower_bound = iter.size_hint().0 + arr.next + 1;
            if slc.capacity() < lower_bound {
                slc.reserve(lower_bound - slc.capacity());
            }
            for index in 0..arr.next {
                slc.push(unsafe { arr.arr[index].assume_init_read() });
            }
            arr.next = 0;
            slc.push(item);
            slc.extend(iter);
            self.slc = Some(NonNull::from(slc));
            return self;
        }

        self
    }

    pub fn view(&self) -> &[T] {
        if let Some(slc) = self.slc {
            return unsafe { slc.as_ref().as_slice() };
        }
        if let Some(arr) = self.arr {
            let arr = unsafe { arr.as_ref() };
            return unsafe { slice::from_raw_parts(arr.arr.as_ptr() as *const T, arr.next) };
        }
        &[]
    }

    pub fn cut(&mut self, n: usize) {
        if let Some(mut slc) = self.slc {
            unsafe { slc.as_mut().truncate(n) };
        } else if let Some(mut arr) = self.arr {
            let arr = unsafe { arr.as_mut() };
            for index in n..arr.next {
                unsafe { arr.arr[index].assume_init_drop() };
            }
            arr.next = n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IDedPool, Pool, VarLength, VarLengthPool, ARRAY_SIZE};

    #[derive(Default, Debug, Eq, PartialEq)]
    struct Item {
        value: usize,
    }

    #[test]
    fn pool_reuses_pages_and_resets_items() {
        let mut pool = Pool::new(Some(|item: &mut Item| item.value = 7));
        let first_ptr = pool.allocate() as *mut Item;
        assert_eq!(unsafe { &*first_ptr }.value, 7);
        pool.reset();
        let second_ptr = pool.allocate() as *mut Item;
        assert_eq!(first_ptr, second_ptr);
        assert_eq!(unsafe { &*second_ptr }.value, 7);
    }

    #[test]
    fn ided_pool_gets_same_item_by_id() {
        let mut pool = IDedPool::new(Some(|item: &mut Item| item.value = 0));
        pool.get_or_allocate(4).value = 12;
        assert_eq!(pool.get(4).unwrap().value, 12);
        assert!(pool.get(5).is_none());
        assert_eq!(pool.max_id_encountered(), 4);
        pool.reset();
        assert!(pool.get(4).is_none());
        assert_eq!(pool.max_id_encountered(), -1);
    }

    #[test]
    fn nil_var_length_grows_and_views() {
        let mut pool = VarLengthPool::<u64>::new();
        let value = VarLength::new_nil().append(&mut pool, [1]);
        assert_eq!(value.view(), &[1]);
    }

    #[test]
    fn var_length_allocate_and_reuse() {
        let mut pool = VarLengthPool::<u64>::new();
        let mut short = pool.allocate(5);
        short = short.append(&mut pool, 0..ARRAY_SIZE as u64);
        assert_eq!(short.view(), &[0, 1, 2, 3, 4, 5, 6, 7]);

        let mut long = pool.allocate(25);
        long = long.append(&mut pool, 0..10);
        long = long.append(&mut pool, 0..20);
        assert_eq!(long.view().len(), 30);
        long.cut(5);
        assert_eq!(long.view(), &[0, 1, 2, 3, 4]);

        pool.reset();
        let reused = pool.allocate(25);
        assert!(reused.view().is_empty());
    }
}
