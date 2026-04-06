use std::ptr;

use crate::ssa::Type;

pub fn spill_slot_size(ty: Type) -> u32 {
    if ty == Type::V128 {
        16
    } else {
        8
    }
}

pub unsafe fn unwind_stack(rbp: usize, top: usize, return_addresses: &mut Vec<usize>) {
    let mut cur = rbp;
    while cur != 0 && cur < top {
        let caller_rbp = ptr::read_unaligned(cur as *const u64) as usize;
        let ret = ptr::read_unaligned((cur + 8) as *const u64) as usize;
        return_addresses.push(ret);
        if caller_rbp == 0 {
            break;
        }
        cur = caller_rbp;
    }
}

pub unsafe fn adjust_cloned_stack(
    old_rsp: usize,
    old_top: usize,
    rsp: usize,
    rbp: usize,
    top: usize,
) {
    let diff = rsp.wrapping_sub(old_rsp);
    let mut cur = rbp;
    while cur != 0 && cur < top {
        let caller_rbp = ptr::read_unaligned(cur as *const u64) as usize;
        if caller_rbp == 0 {
            break;
        }
        assert!(
            caller_rbp >= old_rsp && caller_rbp < old_top,
            "caller rbp out of range"
        );
        ptr::write_unaligned(cur as *mut u64, caller_rbp.wrapping_add(diff) as u64);
        cur = caller_rbp.wrapping_add(diff);
    }
}

#[cfg(test)]
mod tests {
    use super::{adjust_cloned_stack, spill_slot_size, unwind_stack};
    use crate::ssa::Type;

    #[test]
    fn spill_slot_sizes_match_go() {
        assert_eq!(spill_slot_size(Type::I64), 8);
        assert_eq!(spill_slot_size(Type::V128), 16);
    }

    #[test]
    fn stack_helpers_walk_rbp_chain() {
        let mut stack = vec![0u8; 64];
        let base = stack.as_mut_ptr() as usize;
        unsafe {
            std::ptr::write_unaligned(base as *mut u64, (base + 16) as u64);
            std::ptr::write_unaligned((base + 8) as *mut u64, 0x1111);
            std::ptr::write_unaligned((base + 16) as *mut u64, 0);
            std::ptr::write_unaligned((base + 24) as *mut u64, 0x2222);
            let mut frames = Vec::new();
            unwind_stack(base, base + stack.len(), &mut frames);
            assert_eq!(frames, vec![0x1111, 0x2222]);
        }
    }

    #[test]
    fn cloned_stack_adjusts_links() {
        let mut old = vec![0u8; 64];
        let old_base = old.as_mut_ptr() as usize;
        unsafe {
            std::ptr::write_unaligned(old_base as *mut u64, (old_base + 16) as u64);
            std::ptr::write_unaligned((old_base + 16) as *mut u64, 0);
        }
        let mut new = old.clone();
        let new_base = new.as_mut_ptr() as usize;
        unsafe {
            adjust_cloned_stack(
                old_base,
                old_base + old.len(),
                new_base,
                new_base,
                new_base + new.len(),
            );
            assert_eq!(
                std::ptr::read_unaligned(new_base as *const u64) as usize,
                new_base + 16
            );
        }
    }
}
