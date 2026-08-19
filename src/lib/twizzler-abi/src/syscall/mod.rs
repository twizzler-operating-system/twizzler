//! Wrapper functions around for raw_syscall, providing a typed and safer way to interact with the
//! kernel.

mod console;
mod create;
mod handle;
mod info;
mod kaction;
mod map;
mod map_control;
mod object_control;
mod object_stat;
mod random;
mod security;
mod spawn;
mod thread_control;
mod thread_sync;
mod time;
mod trace;

use crate::arch::syscall::raw_syscall;
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    /// Gets random bytes
    GetRandom,
    /// Manipulate mappings
    MapCtrl,
    /// Manage tracing
    Ktrace,
    Enumerate,
    /// Copy ranges into, or zero ranges within, an object that already exists.
    ObjectCopy,
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
pub use map_control::*;
pub use object_control::*;
pub use object_stat::*;
pub use random::*;
pub use security::*;
pub use spawn::*;
pub use thread_control::*;
pub use thread_sync::*;
pub use time::*;
pub use trace::*;
use twizzler_rt_abi::{
    error::{RawTwzError, TwzError},
    object::ObjID,
};

#[inline]
fn convert_codes_to_result<T, E, D, F, G>(code: u64, val: u64, d: D, f: F, g: G) -> Result<T, E>
where
    F: FnOnce(u64, u64) -> T,
    G: FnOnce(u64, u64) -> E,
    D: FnOnce(u64, u64) -> bool,
{
    if d(code, val) {
        Err(g(code, val))
    } else {
        Ok(f(code, val))
    }
}

#[inline]
fn twzerr(_: u64, v: u64) -> TwzError {
    RawTwzError::new(v).error()
}

/// Shutdown the computer.
#[deprecated]
pub fn sys_debug_shutdown(code: u32) {
    unsafe {
        raw_syscall(Syscall::Null, &[0x12345678, code as u64]);
    }
}

/// Ask the kernel to print the deltas of its internal profiles since the last mark, or (with
/// `rebaseline`) to silently start a new interval. A diagnostic for phased workloads; a kernel
/// built without the profiles prints nothing.
pub fn sys_debug_perfmark(rebaseline: bool) {
    unsafe {
        raw_syscall(Syscall::Null, &[0x12345679, rebaseline as u64]);
    }
}

pub enum EnumerateKind {
    Objects,
    Threads,
    /// Slots of the caller's address space that have an object mapped into them.
    MappedSlots,
}

impl From<EnumerateKind> for u64 {
    fn from(x: EnumerateKind) -> Self {
        match x {
            EnumerateKind::Objects => 0,
            EnumerateKind::Threads => 1,
            EnumerateKind::MappedSlots => 2,
        }
    }
}

impl TryFrom<u64> for EnumerateKind {
    type Error = TwzError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(EnumerateKind::Objects),
            1 => Ok(EnumerateKind::Threads),
            2 => Ok(EnumerateKind::MappedSlots),
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }
}

/// Fill `buf` with the numbers of the address-space slots that currently have an object mapped
/// into them, in ascending order, skipping the first `offset` of them. Returns how many were
/// written; fewer than `buf.len()` means the enumeration is complete.
///
/// The alternative is one [sys_object_read_map] per slot, and there are `SLOTS` (128Ki on x86_64)
/// of them -- which is what `bootstrap` used to do, at 88% of all the syscalls in a boot.
pub fn sys_enumerate_slots(buf: &mut [u64], offset: usize) -> Result<usize, TwzError> {
    let args = [
        EnumerateKind::MappedSlots.into(),
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        offset as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::Enumerate, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, twzerr)
}

pub fn sys_enumerate(
    kind: EnumerateKind,
    buf: &mut [ObjID],
    offset: usize,
) -> Result<usize, TwzError> {
    let kind = kind.into();
    let args = [
        kind,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        offset as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::Enumerate, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, twzerr)
}
