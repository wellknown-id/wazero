#![doc = "Minimal C FFI surface for the razero Rust port."]

use std::{
    cell::RefCell,
    ffi::{c_char, CStr, CString},
    ptr, slice,
};

use razero::{CompiledModule, Context, Instance, ModuleConfig, Runtime, RuntimeConfig};

static VERSION_BYTES: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();
const WASM_PAGE_SIZE: u64 = 65_536;
const MAX_MEMORY_LIMIT_PAGES: u32 = 65_536;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = RefCell::new(None);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RazeroStatus {
    Ok = 0,
    NullArgument = 1,
    InvalidUtf8 = 2,
    BufferTooSmall = 3,
    CompileError = 4,
    InstantiateError = 5,
}

#[repr(C)]
pub struct RazeroRuntimeHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RazeroRuntimeConfigHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RazeroModuleConfigHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RazeroCompiledModuleHandle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct RazeroInstanceHandle {
    _private: [u8; 0],
}

struct RuntimeHandle {
    runtime: Runtime,
}

struct RuntimeConfigHandle {
    config: RuntimeConfig,
}

struct ModuleConfigHandle {
    config: ModuleConfig,
}

struct CompiledModuleHandle {
    module: CompiledModule,
}

struct InstanceHandle {
    instance: Instance,
}

fn clear_last_error() {
    LAST_ERROR.with(|last_error| {
        *last_error.borrow_mut() = None;
    });
}

fn set_last_error(message: impl AsRef<str>) {
    let message = message.as_ref().replace('\0', " ");
    let message = CString::new(message).expect("NUL bytes are stripped from error messages");

    LAST_ERROR.with(|last_error| {
        *last_error.borrow_mut() = Some(message);
    });
}

fn ok() -> RazeroStatus {
    clear_last_error();
    RazeroStatus::Ok
}

fn fail(status: RazeroStatus, message: impl AsRef<str>) -> RazeroStatus {
    set_last_error(message);
    status
}

unsafe fn c_string(value: *const c_char) -> Result<Option<String>, RazeroStatus> {
    if value.is_null() {
        return Ok(None);
    }

    CStr::from_ptr(value)
        .to_str()
        .map(|value| Some(value.to_owned()))
        .map_err(|_| {
            fail(
                RazeroStatus::InvalidUtf8,
                "string arguments must be valid UTF-8",
            )
        })
}

unsafe fn handle_ref<'a, T, O>(handle: *const O) -> Option<&'a T> {
    handle.cast::<T>().as_ref()
}

unsafe fn handle_mut<'a, T, O>(handle: *mut O) -> Option<&'a mut T> {
    handle.cast::<T>().as_mut()
}

unsafe fn drop_handle<T, O>(handle: *mut O) {
    if !handle.is_null() {
        drop(Box::from_raw(handle.cast::<T>()));
    }
}

fn into_handle<T, O>(value: T) -> *mut O {
    Box::into_raw(Box::new(value)).cast::<O>()
}

unsafe fn write_value<T: Copy>(out: *mut T, value: T, label: &str) -> Result<(), RazeroStatus> {
    let Some(out) = out.as_mut() else {
        return Err(fail(
            RazeroStatus::NullArgument,
            format!("{label} must not be null"),
        ));
    };
    *out = value;
    Ok(())
}

unsafe fn copy_string_into_buffer(
    value: &str,
    buffer: *mut c_char,
    buffer_len: usize,
    label: &str,
) -> Result<(), RazeroStatus> {
    if buffer.is_null() {
        return Err(fail(
            RazeroStatus::NullArgument,
            format!("{label} must not be null"),
        ));
    }

    let bytes = value.as_bytes();
    if buffer_len < bytes.len() + 1 {
        return Err(fail(
            RazeroStatus::BufferTooSmall,
            format!("{label} is too small"),
        ));
    }

    ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), buffer, bytes.len());
    *buffer.add(bytes.len()) = 0;
    Ok(())
}

unsafe fn copy_bytes_into_buffer(
    value: &[u8],
    buffer: *mut u8,
    buffer_len: usize,
    label: &str,
) -> Result<(), RazeroStatus> {
    if buffer.is_null() {
        return Err(fail(
            RazeroStatus::NullArgument,
            format!("{label} must not be null"),
        ));
    }

    if buffer_len < value.len() {
        return Err(fail(
            RazeroStatus::BufferTooSmall,
            format!("{label} is too small"),
        ));
    }

    ptr::copy_nonoverlapping(value.as_ptr(), buffer, value.len());
    Ok(())
}

#[no_mangle]
pub extern "C" fn razero_version() -> *const c_char {
    VERSION_BYTES.as_ptr().cast()
}

#[no_mangle]
pub extern "C" fn razero_last_error_length() -> usize {
    LAST_ERROR.with(|last_error| {
        last_error
            .borrow()
            .as_ref()
            .map_or(0, |message| message.as_bytes().len())
    })
}

#[no_mangle]
pub unsafe extern "C" fn razero_last_error_copy(
    buffer: *mut c_char,
    buffer_len: usize,
) -> RazeroStatus {
    let Some(message) = LAST_ERROR.with(|last_error| last_error.borrow().clone()) else {
        return copy_string_into_buffer("", buffer, buffer_len, "error buffer")
            .map(|()| RazeroStatus::Ok)
            .unwrap_or_else(|status| status);
    };

    copy_string_into_buffer(
        message
            .to_str()
            .expect("stored FFI error messages must be valid UTF-8"),
        buffer,
        buffer_len,
        "error buffer",
    )
    .map(|()| RazeroStatus::Ok)
    .unwrap_or_else(|status| status)
}

#[no_mangle]
pub extern "C" fn razero_runtime_config_new() -> *mut RazeroRuntimeConfigHandle {
    into_handle::<_, RazeroRuntimeConfigHandle>(RuntimeConfigHandle {
        config: RuntimeConfig::new(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_config_enable_secure_mode(
    config: *mut RazeroRuntimeConfigHandle,
    enabled: bool,
) -> RazeroStatus {
    let Some(config) = handle_mut::<RuntimeConfigHandle, _>(config) else {
        return fail(
            RazeroStatus::NullArgument,
            "runtime config handle must not be null",
        );
    };

    config.config = config.config.clone().with_secure_mode(enabled);
    ok()
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_config_set_memory_limit(
    config: *mut RazeroRuntimeConfigHandle,
    bytes: u64,
) -> RazeroStatus {
    let Some(config) = handle_mut::<RuntimeConfigHandle, _>(config) else {
        return fail(
            RazeroStatus::NullArgument,
            "runtime config handle must not be null",
        );
    };

    let pages = bytes
        .div_ceil(WASM_PAGE_SIZE)
        .min(u64::from(MAX_MEMORY_LIMIT_PAGES)) as u32;
    config.config = config.config.clone().with_memory_limit_pages(pages);
    ok()
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_config_enable_fuel(
    config: *mut RazeroRuntimeConfigHandle,
    enabled: bool,
) -> RazeroStatus {
    let Some(config) = handle_mut::<RuntimeConfigHandle, _>(config) else {
        return fail(
            RazeroStatus::NullArgument,
            "runtime config handle must not be null",
        );
    };

    let fuel = if enabled { i64::MAX } else { 0 };
    config.config = config.config.clone().with_fuel(fuel);
    ok()
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_config_enable_close_on_context_done(
    config: *mut RazeroRuntimeConfigHandle,
    enabled: bool,
) -> RazeroStatus {
    let Some(config) = handle_mut::<RuntimeConfigHandle, _>(config) else {
        return fail(
            RazeroStatus::NullArgument,
            "runtime config handle must not be null",
        );
    };

    config.config = config.config.clone().with_close_on_context_done(enabled);
    ok()
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_config_free(config: *mut RazeroRuntimeConfigHandle) {
    drop_handle::<RuntimeConfigHandle, _>(config);
}

#[no_mangle]
pub extern "C" fn razero_module_config_new() -> *mut RazeroModuleConfigHandle {
    into_handle::<_, RazeroModuleConfigHandle>(ModuleConfigHandle {
        config: ModuleConfig::new(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn razero_module_config_set_name(
    config: *mut RazeroModuleConfigHandle,
    name: *const c_char,
) -> RazeroStatus {
    let Some(config) = handle_mut::<ModuleConfigHandle, _>(config) else {
        return fail(
            RazeroStatus::NullArgument,
            "module config handle must not be null",
        );
    };

    let name = match c_string(name) {
        Ok(name) => name,
        Err(status) => return status,
    };

    let Some(name) = name else {
        config.config = ModuleConfig::new();
        return ok();
    };

    config.config = ModuleConfig::new().with_name(name);
    ok()
}

#[no_mangle]
pub unsafe extern "C" fn razero_module_config_free(config: *mut RazeroModuleConfigHandle) {
    drop_handle::<ModuleConfigHandle, _>(config);
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_new(
    config: *const RazeroRuntimeConfigHandle,
    out_runtime: *mut *mut RazeroRuntimeHandle,
) -> RazeroStatus {
    let Some(out_runtime) = out_runtime.as_mut() else {
        return fail(
            RazeroStatus::NullArgument,
            "output runtime handle must not be null",
        );
    };
    *out_runtime = ptr::null_mut();

    let config = handle_ref::<RuntimeConfigHandle, _>(config)
        .map(|config| config.config.clone())
        .unwrap_or_else(RuntimeConfig::new);

    *out_runtime = into_handle::<_, RazeroRuntimeHandle>(RuntimeHandle {
        runtime: Runtime::with_config(config),
    });
    ok()
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_free(runtime: *mut RazeroRuntimeHandle) {
    drop_handle::<RuntimeHandle, _>(runtime);
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_compile(
    runtime: *const RazeroRuntimeHandle,
    wasm_bytes: *const u8,
    wasm_len: usize,
    out_module: *mut *mut RazeroCompiledModuleHandle,
) -> RazeroStatus {
    let Some(out_module) = out_module.as_mut() else {
        return fail(
            RazeroStatus::NullArgument,
            "output compiled module handle must not be null",
        );
    };
    *out_module = ptr::null_mut();

    let Some(runtime) = handle_ref::<RuntimeHandle, _>(runtime) else {
        return fail(
            RazeroStatus::NullArgument,
            "runtime handle must not be null",
        );
    };

    if wasm_len > 0 && wasm_bytes.is_null() {
        return fail(
            RazeroStatus::NullArgument,
            "wasm bytes pointer must not be null when length is non-zero",
        );
    }

    let wasm = if wasm_len == 0 {
        &[]
    } else {
        slice::from_raw_parts(wasm_bytes, wasm_len)
    };

    match runtime.runtime.compile(wasm) {
        Ok(module) => {
            *out_module =
                into_handle::<_, RazeroCompiledModuleHandle>(CompiledModuleHandle { module });
            ok()
        }
        Err(err) => fail(RazeroStatus::CompileError, err.message()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_module_bytes_len(
    module: *const RazeroCompiledModuleHandle,
    out_len: *mut usize,
) -> RazeroStatus {
    let Some(module) = handle_ref::<CompiledModuleHandle, _>(module) else {
        return fail(
            RazeroStatus::NullArgument,
            "compiled module handle must not be null",
        );
    };

    match write_value(
        out_len,
        module.module.bytes().len(),
        "module byte length output",
    ) {
        Ok(()) => ok(),
        Err(status) => status,
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_module_bytes_copy(
    module: *const RazeroCompiledModuleHandle,
    buffer: *mut u8,
    buffer_len: usize,
) -> RazeroStatus {
    let Some(module) = handle_ref::<CompiledModuleHandle, _>(module) else {
        return fail(
            RazeroStatus::NullArgument,
            "compiled module handle must not be null",
        );
    };

    match copy_bytes_into_buffer(
        module.module.bytes(),
        buffer,
        buffer_len,
        "module byte buffer",
    ) {
        Ok(()) => ok(),
        Err(status) => status,
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_module_free(module: *mut RazeroCompiledModuleHandle) {
    drop_handle::<CompiledModuleHandle, _>(module);
}

#[no_mangle]
pub unsafe extern "C" fn razero_runtime_instantiate(
    runtime: *const RazeroRuntimeHandle,
    module: *const RazeroCompiledModuleHandle,
    config: *const RazeroModuleConfigHandle,
    out_instance: *mut *mut RazeroInstanceHandle,
) -> RazeroStatus {
    let Some(out_instance) = out_instance.as_mut() else {
        return fail(
            RazeroStatus::NullArgument,
            "output instance handle must not be null",
        );
    };
    *out_instance = ptr::null_mut();

    let Some(runtime) = handle_ref::<RuntimeHandle, _>(runtime) else {
        return fail(
            RazeroStatus::NullArgument,
            "runtime handle must not be null",
        );
    };
    let Some(module) = handle_ref::<CompiledModuleHandle, _>(module) else {
        return fail(
            RazeroStatus::NullArgument,
            "compiled module handle must not be null",
        );
    };

    let config = handle_ref::<ModuleConfigHandle, _>(config)
        .map(|config| config.config.clone())
        .unwrap_or_else(ModuleConfig::new);

    match runtime.runtime.instantiate(&module.module, config) {
        Ok(instance) => {
            *out_instance = into_handle::<_, RazeroInstanceHandle>(InstanceHandle { instance });
            ok()
        }
        Err(err) => fail(RazeroStatus::InstantiateError, err.message()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_instance_name_len(
    instance: *const RazeroInstanceHandle,
    out_len: *mut usize,
) -> RazeroStatus {
    let Some(instance) = handle_ref::<InstanceHandle, _>(instance) else {
        return fail(
            RazeroStatus::NullArgument,
            "instance handle must not be null",
        );
    };

    match write_value(
        out_len,
        instance.instance.name().map_or(0, str::len),
        "instance name length output",
    ) {
        Ok(()) => ok(),
        Err(status) => status,
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_instance_name_copy(
    instance: *const RazeroInstanceHandle,
    buffer: *mut c_char,
    buffer_len: usize,
) -> RazeroStatus {
    let Some(instance) = handle_ref::<InstanceHandle, _>(instance) else {
        return fail(
            RazeroStatus::NullArgument,
            "instance handle must not be null",
        );
    };

    match copy_string_into_buffer(
        instance.instance.name().unwrap_or(""),
        buffer,
        buffer_len,
        "instance name buffer",
    ) {
        Ok(()) => ok(),
        Err(status) => status,
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_instance_is_closed(
    instance: *const RazeroInstanceHandle,
    out_closed: *mut bool,
) -> RazeroStatus {
    let Some(instance) = handle_ref::<InstanceHandle, _>(instance) else {
        return fail(
            RazeroStatus::NullArgument,
            "instance handle must not be null",
        );
    };

    match write_value(
        out_closed,
        instance.instance.is_closed(),
        "instance closed output",
    ) {
        Ok(()) => ok(),
        Err(status) => status,
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_instance_close(
    instance: *mut RazeroInstanceHandle,
) -> RazeroStatus {
    let Some(instance) = handle_mut::<InstanceHandle, _>(instance) else {
        return fail(
            RazeroStatus::NullArgument,
            "instance handle must not be null",
        );
    };

    match instance.instance.close(&Context::default()) {
        Ok(()) => ok(),
        Err(err) => fail(RazeroStatus::InstantiateError, err.to_string()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn razero_instance_free(instance: *mut RazeroInstanceHandle) {
    drop_handle::<InstanceHandle, _>(instance);
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_WASM: &[u8] = b"\0asm\x01\0\0\0";

    fn last_error() -> String {
        let len = razero_last_error_length();
        let mut buffer = vec![0i8; len + 1];
        let status = unsafe { razero_last_error_copy(buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(RazeroStatus::Ok, status);
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn version_is_a_c_string() {
        let version = unsafe { CStr::from_ptr(razero_version()) };
        assert_eq!(env!("CARGO_PKG_VERSION"), version.to_str().unwrap());
    }

    #[test]
    fn ffi_round_trip_compile_and_instantiate() {
        unsafe {
            let runtime_config = razero_runtime_config_new();
            assert!(!runtime_config.is_null());
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_enable_secure_mode(runtime_config, true)
            );
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_set_memory_limit(runtime_config, 64 * 1024)
            );
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_enable_fuel(runtime_config, true)
            );
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_enable_close_on_context_done(runtime_config, true)
            );

            let module_config = razero_module_config_new();
            assert!(!module_config.is_null());
            let name = CString::new("ffi-smoke").unwrap();
            assert_eq!(
                RazeroStatus::Ok,
                razero_module_config_set_name(module_config, name.as_ptr())
            );

            let mut runtime = ptr::null_mut();
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_new(runtime_config, &mut runtime)
            );
            assert!(!runtime.is_null());

            let mut module = ptr::null_mut();
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_compile(runtime, VALID_WASM.as_ptr(), VALID_WASM.len(), &mut module)
            );
            assert!(!module.is_null());

            let mut module_len = 0;
            assert_eq!(
                RazeroStatus::Ok,
                razero_module_bytes_len(module, &mut module_len)
            );
            assert_eq!(VALID_WASM.len(), module_len);

            let mut copied_bytes = vec![0u8; module_len];
            assert_eq!(
                RazeroStatus::Ok,
                razero_module_bytes_copy(module, copied_bytes.as_mut_ptr(), copied_bytes.len())
            );
            assert_eq!(VALID_WASM, copied_bytes);

            let mut instance = ptr::null_mut();
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_instantiate(runtime, module, module_config, &mut instance)
            );
            assert!(!instance.is_null());

            let mut name_len = 0;
            assert_eq!(
                RazeroStatus::Ok,
                razero_instance_name_len(instance, &mut name_len)
            );
            assert_eq!("ffi-smoke".len(), name_len);

            let mut name_buffer = vec![0i8; name_len + 1];
            assert_eq!(
                RazeroStatus::Ok,
                razero_instance_name_copy(instance, name_buffer.as_mut_ptr(), name_buffer.len())
            );
            let copied_name = CStr::from_ptr(name_buffer.as_ptr()).to_str().unwrap();
            assert_eq!("ffi-smoke", copied_name);

            let mut closed = true;
            assert_eq!(
                RazeroStatus::Ok,
                razero_instance_is_closed(instance, &mut closed)
            );
            assert!(!closed);

            assert_eq!(RazeroStatus::Ok, razero_instance_close(instance));
            assert_eq!(
                RazeroStatus::Ok,
                razero_instance_is_closed(instance, &mut closed)
            );
            assert!(closed);

            razero_instance_free(instance);
            razero_module_free(module);
            razero_runtime_free(runtime);
            razero_module_config_free(module_config);
            razero_runtime_config_free(runtime_config);
        }
    }

    #[test]
    fn runtime_config_bridges_public_api_options() {
        unsafe {
            let runtime_config = razero_runtime_config_new();
            assert!(!runtime_config.is_null());

            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_set_memory_limit(runtime_config, WASM_PAGE_SIZE)
            );
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_enable_fuel(runtime_config, true)
            );

            let config = handle_ref::<RuntimeConfigHandle, _>(runtime_config).unwrap();
            assert_eq!(1, config.config.memory_limit_pages());
            assert_eq!(i64::MAX, config.config.fuel());

            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_config_enable_fuel(runtime_config, false)
            );
            let config = handle_ref::<RuntimeConfigHandle, _>(runtime_config).unwrap();
            assert_eq!(0, config.config.fuel());

            razero_runtime_config_free(runtime_config);
        }
    }

    #[test]
    fn compile_error_surfaces_through_last_error() {
        unsafe {
            let mut runtime = ptr::null_mut();
            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_new(ptr::null(), &mut runtime)
            );

            let mut module = ptr::null_mut();
            let status = razero_runtime_compile(runtime, ptr::null(), 0, &mut module);
            assert_eq!(RazeroStatus::CompileError, status);
            assert!(module.is_null());
            assert!(
                last_error().contains("invalid magic number"),
                "unexpected error: {}",
                last_error()
            );

            let module_config = razero_module_config_new();
            assert_eq!(
                RazeroStatus::Ok,
                razero_module_config_set_name(module_config, ptr::null())
            );
            assert_eq!(0, razero_last_error_length());

            razero_module_config_free(module_config);
            razero_runtime_free(runtime);
        }
    }

    #[test]
    fn reports_invalid_arguments_and_utf8() {
        unsafe {
            let mut runtime = ptr::null_mut();
            assert_eq!(
                RazeroStatus::NullArgument,
                razero_runtime_new(ptr::null(), ptr::null_mut())
            );
            assert!(
                last_error().contains("output runtime handle must not be null"),
                "unexpected error: {}",
                last_error()
            );

            assert_eq!(
                RazeroStatus::Ok,
                razero_runtime_new(ptr::null(), &mut runtime)
            );

            let invalid_utf8 = [0xff, 0];
            let module_config = razero_module_config_new();
            let status = razero_module_config_set_name(
                module_config,
                invalid_utf8.as_ptr().cast::<c_char>(),
            );
            assert_eq!(RazeroStatus::InvalidUtf8, status);
            assert!(
                last_error().contains("valid UTF-8"),
                "unexpected error: {}",
                last_error()
            );

            razero_module_config_free(module_config);
            razero_runtime_free(runtime);
        }
    }
}
