#![doc = "Compiler host module glue."]

use std::mem;
use std::sync::Arc;

use razero_wasm::host_func::{HostFuncRef, HostFunction};
use razero_wasm::module::Module;

use crate::engine::AlignedBytes;

const HOST_MODULE_HEADER_SIZE: usize = 32;
const HOST_FUNCTION_SLOT_SIZE: usize = 16;

pub(crate) fn build_host_module_opaque(module: &Module) -> AlignedBytes {
    let size = HOST_MODULE_HEADER_SIZE + module.code_section.len() * HOST_FUNCTION_SLOT_SIZE;
    let mut opaque = AlignedBytes::zeroed(size.max(HOST_MODULE_HEADER_SIZE));
    write_usize(opaque.as_mut_slice(), 0, module as *const Module as usize);

    let mut offset = HOST_MODULE_HEADER_SIZE;
    for code in &module.code_section {
        let host_func = code
            .host_func
            .as_ref()
            .unwrap_or_else(|| panic!("host module function missing host implementation"));
        write_host_func_ref(host_func, &mut opaque.as_mut_slice()[offset..offset + HOST_FUNCTION_SLOT_SIZE]);
        offset += HOST_FUNCTION_SLOT_SIZE;
    }
    opaque
}

#[allow(dead_code)]
pub(crate) unsafe fn host_module_from_opaque<'a>(opaque_begin: usize) -> &'a Module {
    &*(read_usize(opaque_begin as *const u8, 0) as *const Module)
}

#[allow(dead_code)]
pub(crate) unsafe fn host_module_host_func_from_opaque(
    index: usize,
    opaque_begin: usize,
) -> HostFuncRef {
    let base = opaque_begin as *const u8;
    let offset = HOST_MODULE_HEADER_SIZE + index * HOST_FUNCTION_SLOT_SIZE;
    let words = [
        read_usize(base, offset),
        read_usize(base, offset + mem::size_of::<usize>()),
    ];
    let raw: *const dyn HostFunction = mem::transmute(words);
    Arc::increment_strong_count(raw);
    Arc::from_raw(raw)
}

pub(crate) fn write_host_func_ref(host_func: &HostFuncRef, buf: &mut [u8]) {
    assert!(buf.len() >= HOST_FUNCTION_SLOT_SIZE);
    let raw = Arc::as_ptr(host_func);
    let words: [usize; 2] = unsafe { mem::transmute(raw) };
    write_usize(buf, 0, words[0]);
    write_usize(buf, mem::size_of::<usize>(), words[1]);
}

fn write_usize(buf: &mut [u8], offset: usize, value: usize) {
    let bytes = value.to_le_bytes();
    buf[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

#[allow(dead_code)]
fn read_usize(base: *const u8, offset: usize) -> usize {
    let mut bytes = [0u8; mem::size_of::<usize>()];
    unsafe {
        std::ptr::copy_nonoverlapping(base.add(offset), bytes.as_mut_ptr(), bytes.len());
    }
    usize::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use razero_wasm::host_func::{stack_host_func, Caller};
    use razero_wasm::module::{Code, CodeBody, FunctionType, Module, ValueType};

    use super::{build_host_module_opaque, host_module_from_opaque, host_module_host_func_from_opaque};

    #[test]
    fn host_module_opaque_round_trips_module_and_host_funcs() {
        let host = stack_host_func(|stack| {
            stack[0] = stack[0].wrapping_add(1);
            Ok(())
        });
        let mut ty = FunctionType::default();
        ty.params = vec![ValueType::I64];
        ty.results = vec![ValueType::I64];
        ty.cache_num_in_u64();
        let module = Module {
            is_host_module: true,
            type_section: vec![ty],
            function_section: vec![0],
            code_section: vec![Code {
                body_kind: CodeBody::Host,
                host_func: Some(host),
                ..Code::default()
            }],
            ..Module::default()
        };

        let opaque = build_host_module_opaque(&module);
        let begin = opaque.as_ptr() as usize;

        unsafe {
            assert!(std::ptr::eq(host_module_from_opaque(begin), &module));
            let retrieved = host_module_host_func_from_opaque(0, begin);
            let mut caller = Caller::default();
            let mut stack = [41u64];
            retrieved.call(&mut caller, &mut stack).unwrap();
            assert_eq!([42], stack);
        }
    }
}
