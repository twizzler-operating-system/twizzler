//! Wrapper functions around for raw_syscall, providing a typed and safer way to interact with the kernel.

mod console;
mod create;
mod handle;
mod info;
mod kaction;
mod map;
mod object_control;
mod object_stat;
mod spawn;
mod thread_control;
mod thread_sync;
mod time;

use crate::arch::syscall::raw_syscall;
#[derive(Copy, Clone, Debug)]
#[repr(C)]
/// All possible Synchronous syscalls into the Twizzler kernel.
pub enum Syscall {
    Null = 0,
    /// Read data from the kernel console, either buffer or input.
    KernelConsoleRead = 1,
    /// Write data to the kernel console.
    KernelConsoleWrite = 2,
    /// Sync a thread with other threads using some number of memory words.
    ThreadSync = 3,
    /// General thread control functions.
    ThreadCtrl = 4,
    /// Create new object.
    ObjectCreate = 5,
    /// Map an object into address space.
    ObjectMap = 6,
    /// Returns system info.
    SysInfo = 7,
    /// Spawn a new thread.
    Spawn = 8,
    /// Read clock information.
    ReadClockInfo = 9,
    /// Apply a kernel action to an object (used for device drivers).
    Kaction = 10,
    /// New Handle.
    NewHandle = 11,
    /// Unmap an object.
    ObjectUnmap = 12,
    /// Delete an object.
    Delete = 13,
    /// Manage in-kernel object properties.
    ObjectCtrl = 14,
    /// Get kernel information about an object.
    ObjectStat = 15,
    /// Read mapping information.
    ObjectReadMap = 16,
    MaxSyscalls = 19,
}

impl Syscall {
    /// Return the number associated with this syscall.
    pub fn num(&self) -> u64 {
        *self as u64
    }
}

impl From<usize> for Syscall {
    fn from(x: usize) -> Self {
        if x >= Syscall::MaxSyscalls as usize {
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

#[deprecated]
pub fn sys_debug_shutdown(code: u32) {
    unsafe {
        raw_syscall(Syscall::Null, &[0x12345678, code as u64]);
    }
}
