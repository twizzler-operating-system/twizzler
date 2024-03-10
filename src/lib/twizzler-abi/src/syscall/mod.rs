//! Wrapper functions around for raw_syscall, providing a typed and safer way to interact with the kernel.

mod console;
mod create;
mod handle;
mod info;
mod kaction;
mod map;
mod object_control;
mod object_stat;
mod security;
mod spawn;
mod thread_control;
mod thread_sync;
mod time;

use crate::arch::syscall::raw_syscall;
#[derive(Copy, Clone, Debug)]
#[repr(C)]
/// All possible Synchronous syscalls into the Twizzler kernel.
pub enum Syscall {
    Null,
    /// Read data from the kernel console, either buffer or input.
    KernelConsoleRead,
    /// Write data to the kernel console.
    KernelConsoleWrite,
    /// Sync a thread with other threads using some number of memory words.
    ThreadSync,
    /// General thread control functions.
    ThreadCtrl,
    /// Create new object.
    ObjectCreate,
    /// Map an object into address space.
    ObjectMap,
    /// Returns system info.
    SysInfo,
    /// Spawn a new thread.
    Spawn,
    /// Read clock information.
    ReadClockInfo,
    /// List clock sources.
    ReadClockList,
    /// Apply a kernel action to an object (used for device drivers).
    Kaction,
    /// New Handle.
    NewHandle,
    /// Unmap an object.
    ObjectUnmap,
    /// Manage in-kernel object properties.
    ObjectCtrl,
    /// Get kernel information about an object.
    ObjectStat,
    /// Read mapping information.
    ObjectReadMap,
    /// Remove an object as a handle.
    UnbindHandle,
    /// Attach to a security context.
    SctxAttach,
    NumSyscalls,
}

impl Syscall {
    /// Return the number associated with this syscall.
    pub fn num(&self) -> u64 {
        *self as u64
    }
}

impl From<usize> for Syscall {
    fn from(x: usize) -> Self {
        if x >= Syscall::NumSyscalls as usize {
            return Syscall::Null;
        }
        unsafe { core::intrinsics::transmute(x as u32) }
    }
}

pub use console::*;
pub use create::*;
pub use handle::*;
pub use info::*;
pub use kaction::*;
pub use map::*;
pub use object_control::*;
pub use object_stat::*;
pub use security::*;
pub use spawn::*;
pub use thread_control::*;
pub use thread_sync::*;
pub use time::*;

#[inline]
fn convert_codes_to_result<T, E, D, F, G>(code: u64, val: u64, d: D, f: F, g: G) -> Result<T, E>
where
    F: Fn(u64, u64) -> T,
    G: Fn(u64, u64) -> E,
    D: Fn(u64, u64) -> bool,
{
    if d(code, val) {
        Err(g(code, val))
    } else {
        Ok(f(code, val))
    }
}

#[inline]
fn justval<T: From<u64>>(_: u64, v: u64) -> T {
    v.into()
}

/// Shutdown the computer.
#[deprecated]
pub fn sys_debug_shutdown(code: u32) {
    unsafe {
        raw_syscall(Syscall::Null, &[0x12345678, code as u64]);
    }
}
