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

use core::time::Duration;

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
    /// Enumerate kernel-known objects, threads, or mapped slots.
    Enumerate,
    /// Copy ranges into, or zero ranges within, an object that already exists.
    ObjectCopy,
    /// Issue kernel commands
    SysCtrl,
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

/// Whole-system maintenance operations for [sys_ctrl]. Each runs on the calling thread, at the
/// calling thread's priority.
#[derive(Clone, Copy, Debug)]
pub enum SysCtrlCmd {
    /// Print kernel debugging state to the kernel console. Returns 0.
    DebugDump,
    /// Sync every mapping registered with `SYNC_FLAG_ASYNC_DURABLE`, along with any syncs already
    /// queued for the kernel's background sync thread, and then flush the pager's backing store.
    /// Returns the number of objects synced.
    SyncAll,
    /// Zero every free physical frame. Returns the number of bytes zeroed.
    ZeroAll,
    /// Turn kernel diagnostic classes on and off at runtime: `arg1` is a [KernelDiagFlags] mask to
    /// set and `arg2` a mask to clear, applied in that order. Returns the resulting mask, so
    /// passing zero for both reads the current one without changing it.
    SetDiag,
    /// Reap everything awaiting background reclamation -- deleted objects and exited threads --
    /// rather than waiting for the idle loop and the `BACKGROUND` reaper to get to it. Returns the
    /// number of objects and threads reaped.
    ReapAll,
    /// Flush pending work to the backing store and power the machine off, handing the host the
    /// exit code in `arg1`. Does not return. `VERBOSE` dumps the kernel's profile counters first.
    Shutdown,
}

impl TryFrom<u64> for SysCtrlCmd {
    type Error = TwzError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(SysCtrlCmd::DebugDump),
            1 => Ok(SysCtrlCmd::SyncAll),
            2 => Ok(SysCtrlCmd::ZeroAll),
            3 => Ok(SysCtrlCmd::SetDiag),
            4 => Ok(SysCtrlCmd::ReapAll),
            5 => Ok(SysCtrlCmd::Shutdown),
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }
}

impl From<SysCtrlCmd> for u64 {
    fn from(x: SysCtrlCmd) -> Self {
        match x {
            SysCtrlCmd::DebugDump => 0,
            SysCtrlCmd::SyncAll => 1,
            SysCtrlCmd::ZeroAll => 2,
            SysCtrlCmd::SetDiag => 3,
            SysCtrlCmd::ReapAll => 4,
            SysCtrlCmd::Shutdown => 5,
        }
    }
}

bitflags::bitflags! {
    /// Kernel diagnostic classes, as set at boot by `--diag` / `--diag=<list>` and at runtime by
    /// [SysCtrlCmd::SetDiag].
    ///
    /// Each is a read-mostly atomic the instrumented path loads, so a class that is off costs one
    /// load. Being able to move them at runtime is the point: armed for a whole boot, a diagnostic
    /// perturbs every measurement taken alongside it, and the interesting window is usually one
    /// phase of one workload.
    #[derive(Default, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct KernelDiagFlags: u64 {
        /// The idle-loop hang diagnostics: stuck-thread reports, orphan-thread and mutex-timeout
        /// scans. Turning this on at runtime does not enable the page-table zero check, which
        /// boot's bare `--diag` also arms and which cannot be turned back off.
        const DIAG = 1 << 0;
        /// `--diag=pager`: kernel-side pager and large-page milestone reports.
        const PAGER = 1 << 1;
        /// `--diag=invls`: per-object invalidation-latch reports.
        const INVLS = 1 << 2;
        /// `--diag=wake`: wake and scheduling latency reports.
        const WAKE = 1 << 3;
        /// `--diag=fault`: the per-object page-fault census.
        const FAULT = 1 << 4;
    }
}

bitflags::bitflags! {
    /// Flags for [sys_ctrl].
    #[derive(Default, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct SysCtrlFlags: u64 {
        /// If set, the kernel submits the requested work but does not block until it completes:
        /// `SyncAll` does not wait for the pager to acknowledge, and `ZeroAll` makes a single pass
        /// rather than sweeping to completion. Only meaningful for `SyncAll` and `ZeroAll`.
        const NO_WAIT = 1 << 0;
        /// If set, `DebugDump` prints more, including per-object state.
        const VERBOSE = 1 << 1;
    }
}

/// Perform a whole-system maintenance operation; see [SysCtrlCmd] for the commands and their
/// return values.
///
/// `timeout`, when given, bounds `SyncAll` and `ZeroAll`: an operation still unfinished at the
/// deadline returns [TwzError::TIMED_OUT] having done as much as it could. `DebugDump` ignores it.
/// `arg1`-`arg3` are unused today.
pub fn sys_ctrl(
    cmd: SysCtrlCmd,
    timeout: Option<Duration>,
    flags: SysCtrlFlags,
    arg1: u64,
    arg2: u64,
    arg3: u64,
) -> Result<u64, TwzError> {
    let ts = timeout.map(TimeSpan::from);
    // Null-or-pointer-to-the-bare-value, the same convention [sys_thread_sync] uses, because that
    // is what the kernel reads. A `*const Option<TimeSpan>` would be wrong twice over: it is the
    // address of a local and so never null, leaving `None` unrepresentable, and the kernel would
    // read the `Option`'s discriminant as the span's seconds field.
    let ts_ptr = ts
        .as_ref()
        .map_or(core::ptr::null(), |t| t as *const TimeSpan);
    let args = [cmd.into(), ts_ptr as u64, flags.bits(), arg1, arg2, arg3];
    let (code, val) = unsafe { raw_syscall(Syscall::SysCtrl, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, twzerr)
}
