use std::slice;

pub unsafe fn unwind_stack(sp: usize, top: usize, mut return_addresses: Vec<usize>) -> Vec<usize> {
    let len = top - sp;
    let stack = unsafe { slice::from_raw_parts(sp as *const u8, len) };
    let mut index = 0usize;
    while index < len {
        let frame_size = u64::from_le_bytes(stack[index..index + 8].try_into().unwrap()) as usize;
        index += frame_size + 16;
        let ret_addr = u64::from_le_bytes(stack[index..index + 8].try_into().unwrap()) as usize;
        index += 8;
        let arg_ret = u64::from_le_bytes(stack[index..index + 8].try_into().unwrap()) as usize;
        index += 8 + arg_ret;
        return_addresses.push(ret_addr);
    }
    return_addresses
}

pub unsafe fn go_call_stack_view(stack_pointer_before_go_call: *const u64) -> &'static [u64] {
    let size = unsafe { *stack_pointer_before_go_call.add(1) as usize };
    let data = unsafe { stack_pointer_before_go_call.add(2) };
    unsafe { slice::from_raw_parts(data, size) }
}

#[cfg(test)]
mod tests {
    use super::{go_call_stack_view, unwind_stack};

    #[test]
    fn unwind_stack_decodes_frame_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0x1234u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let base = bytes.as_ptr() as usize;
        let top = base + bytes.len();
        let addrs = unsafe { unwind_stack(base, top, Vec::new()) };
        assert_eq!(addrs, vec![0x1234]);
    }

    #[test]
    fn go_call_stack_view_exposes_payload_slice() {
        let data = [0u64, 2, 11, 22];
        let view = unsafe { go_call_stack_view(data.as_ptr()) };
        assert_eq!(view, &[11, 22]);
    }
}
