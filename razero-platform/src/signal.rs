#![doc = "Signal-handler platform abstraction for JIT fault recovery."]

use std::fmt;

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Sigaction {
    pub handler: usize,
    pub flags: u64,
    pub restorer: usize,
    pub mask: u64,
}

#[derive(Debug)]
pub enum SignalError {
    Syscall { operation: &'static str, errno: i32 },
    Unsupported(&'static str),
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syscall { operation, errno } => {
                write!(f, "signal syscall '{}' failed: errno {}", operation, errno)
            }
            Self::Unsupported(reason) => write!(f, "signal handling unsupported: {}", reason),
        }
    }
}

impl std::error::Error for SignalError {}

#[cfg(target_os = "linux")]
pub fn read_sigsegv_handler() -> Result<Sigaction, SignalError> {
    let mut old = Sigaction::default();
    let rc = unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction as libc::c_long,
            libc::SIGSEGV,
            std::ptr::null::<Sigaction>(),
            &mut old as *mut Sigaction,
            8usize,
        )
    };
    if rc == -1 {
        return Err(SignalError::Syscall {
            operation: "rt_sigaction read",
            errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        });
    }
    Ok(old)
}

#[cfg(target_os = "linux")]
pub fn install_sigsegv_handler(handler_addr: usize) -> Result<Sigaction, SignalError> {
    let old = read_sigsegv_handler()?;
    let mut act = old;
    act.handler = handler_addr;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction as libc::c_long,
            libc::SIGSEGV,
            &act as *const Sigaction,
            std::ptr::null_mut::<Sigaction>(),
            8usize,
        )
    };
    if rc == -1 {
        return Err(SignalError::Syscall {
            operation: "rt_sigaction install",
            errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        });
    }
    Ok(old)
}

#[cfg(not(target_os = "linux"))]
pub fn install_sigsegv_handler(_handler_addr: usize) -> Result<Sigaction, SignalError> {
    Err(SignalError::Unsupported("not linux"))
}

#[cfg(not(target_os = "linux"))]
pub fn read_sigsegv_handler() -> Result<Sigaction, SignalError> {
    Err(SignalError::Unsupported("not linux"))
}
