//! Pointer resurrection helpers for low-level runtime interop.

use core::ptr::NonNull;

/// The caller must ensure `ptr` was derived from a valid `T` allocation and remains live.
pub unsafe fn ptr_from_usize<T>(ptr: usize) -> *mut T {
    ptr as *mut T
}

/// The caller must ensure `ptr` is non-zero and was derived from a valid `T` allocation.
pub unsafe fn non_null_from_usize<T>(ptr: usize) -> NonNull<T> {
    NonNull::new(ptr_from_usize(ptr)).expect("non-null pointer")
}

#[cfg(test)]
mod tests {
    use super::{non_null_from_usize, ptr_from_usize};

    #[test]
    fn pointer_helpers_round_trip_addresses() {
        let mut value = 11u64;
        let addr = (&mut value as *mut u64) as usize;
        let ptr = unsafe { ptr_from_usize::<u64>(addr) };
        let non_null = unsafe { non_null_from_usize::<u64>(addr) };
        unsafe {
            *ptr = 42;
            *non_null.as_ptr() += 1;
        }
        assert_eq!(value, 43);
    }
}
