#![doc = "Compiler memmove plumbing."]

use core::ptr;

pub unsafe fn memmove(dst: *mut u8, src: *const u8, len: usize) {
    if len == 0 || std::ptr::eq(dst.cast_const(), src) {
        return;
    }
    ptr::copy(src, dst, len);
}

pub fn memmove_ptr() -> usize {
    memmove as *const () as usize
}

#[cfg(test)]
mod tests {
    use super::{memmove, memmove_ptr};

    #[test]
    fn memmove_pointer_is_non_zero() {
        assert_ne!(memmove_ptr(), 0);
    }

    #[test]
    fn memmove_handles_overlapping_ranges() {
        let mut bytes = *b"abcdef";
        unsafe {
            memmove(bytes[1..].as_mut_ptr(), bytes.as_ptr(), 5);
        }
        assert_eq!(&bytes, b"aabcde");
    }
}
