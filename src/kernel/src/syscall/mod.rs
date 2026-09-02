use core::{
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use object::{map_ctrl, object_ctrl};
use twizzler_abi::{
    kso::{KactionCmd, KactionValue},
    object::{ObjID, Protections},
    syscall::{
        ClockFlags, ClockInfo, ClockKind, ClockSource, FemtoSeconds, GetRandomFlags, HandleType,
        InfoKind, KernelConsoleSource, MapFlags, ReadClockListFlags, Syscall, SyscallStats,
        TimeSpan,
    },
    trace::{
        SyscallEntryEvent, SyscallExitEvent, THREAD_SYSCALL_ENTRY, THREAD_SYSCALL_EXIT,
        TraceEntryFlags, TraceKind,
    },
};
use twizzler_rt_abi::{
    Result,
    error::{ArgumentError, ResourceError, TwzError},
};

use self::{
    object::{sys_new_handle, sys_sctx_attach, sys_unbind_handle},
    thread::thread_ctrl,
};
use crate::{
    clock::{fill_with_every_first, fill_with_first_kind, fill_with_kind},
    instant::Instant,
    memory::VirtAddr,
    processor::mp::{current_processor, with_each_active_processor},
    random::getrandom,
    thread::current_thread_ref,
    time::TimeStatCollector,
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

// TODO: move the handle stuff into its own file and make this private.
pub mod object;
/* TODO: move the requeue stuff into sched and make this private */
mod stat;
pub mod sync;
mod thread;

pub trait SyscallContext {
    fn create_jmp_context(target: VirtAddr, stack: VirtAddr, arg: u64) -> Self;
    fn num(&self) -> usize;
    fn arg0<T: From<u64>>(&self) -> T;
    fn arg1<T: From<u64>>(&self) -> T;
    fn arg2<T: From<u64>>(&self) -> T;
    fn arg3<T: From<u64>>(&self) -> T;
    fn arg4<T: From<u64>>(&self) -> T;
    fn arg5<T: From<u64>>(&self) -> T;
    fn pc(&self) -> VirtAddr;
    fn set_return_values<R1, R2>(&mut self, ret0: R1, ret1: R2)
    where
        u64: From<R1>,
        u64: From<R2>;
    fn get_return_values<R1, R2>(&mut self) -> (R1, R2)
    where
        R1: From<u64>,
        R2: From<u64>;
}

pub unsafe fn create_user_slice<'a, T>(ptr: u64, len: u64) -> Option<&'a mut [T]> {
    /* TODO: verify pointers */
    unsafe { Some(core::slice::from_raw_parts_mut(ptr as *mut T, len as usize)) }
}

unsafe fn create_user_ptr<'a, T>(ptr: u64) -> Option<&'a mut T> {
    unsafe { (ptr as *mut T).as_mut() }
}

unsafe fn create_user_nullable_ptr<'a, T>(ptr: u64) -> Option<Option<&'a mut T>> {
    unsafe { Some((ptr as *mut T).as_mut()) }
}

fn sys_kernel_console_write(
    target: KernelConsoleSource,
    data: &[u8],
    flags: twizzler_abi::syscall::KernelConsoleWriteFlags,
) {
    let _res = crate::log::write_bytes(target, data, flags.into());
}

fn type_sys_object_create(
    create: u64,
    src_ptr: u64,
    src_len: u64,
    tie_ptr: u64,
    tie_len: u64,
) -> Result<ObjID> {
    let srcs =
        unsafe { create_user_slice(src_ptr, src_len) }.ok_or(ArgumentError::InvalidArgument)?;
    let ties =
        unsafe { create_user_slice(tie_ptr, tie_len) }.ok_or(ArgumentError::InvalidArgument)?;
    let create = unsafe { create_user_ptr(create) }.ok_or(ArgumentError::InvalidArgument)?;
    object::sys_object_create(create, srcs, ties)
}

fn type_sys_object_copy(id: ObjID, src_ptr: u64, src_len: u64) -> Result<()> {
    let srcs =
        unsafe { create_user_slice(src_ptr, src_len) }.ok_or(ArgumentError::InvalidArgument)?;
    object::sys_object_copy(id, srcs)
}

fn type_sys_thread_sync(ptr: u64, len: u64, timeoutptr: u64) -> Result<usize> {
    let slice = unsafe { create_user_slice(ptr, len) }.ok_or(ArgumentError::InvalidArgument)?;
    let timeout =
        unsafe { create_user_nullable_ptr(timeoutptr) }.ok_or(ArgumentError::InvalidArgument)?;
    sync::sys_thread_sync(slice, timeout)
}

fn write_sysinfo(info: *mut u8, kind: u64) -> Result<()> {
    let kind: InfoKind = kind.try_into()?;
    stat::write_sys_info_values(info, kind)
}

fn type_sys_kaction(
    cmd: u64,
    hi: u64,
    lo: u64,
    arg: u64,
    _flags: u64,
    arg2: u64,
) -> Result<KactionValue> {
    let cmd = KactionCmd::try_from(cmd)?;
    let objid = if hi == 0 {
        None
    } else {
        Some(ObjID::from_parts([hi, lo]))
    };
    crate::device::kaction(cmd, objid, arg, arg2)
}

fn type_read_clock_info(src: u64, info: u64, _flags: u64) -> Result<u64> {
    let source: ClockSource = src.into();
    let info_ptr: &mut MaybeUninit<ClockInfo> =
        unsafe { create_user_ptr(info) }.ok_or(ArgumentError::InvalidArgument)?;

    // Through the cache, not `TICK_SOURCES.lock()`. Every arm of this used to take that one global
    // spinlock, so userspace asking the time serialized every cpu in the system against every
    // other -- the same defect `Instant::now()` had and fixed (see `instant.rs`), left behind on
    // the syscall path. Userspace clocks now calibrate once and read the tick counter themselves,
    // so this is no longer hot, but it is reachable by anything that cannot do that and there is
    // no reason for it to hold a lock: tick sources are registered at boot and never replaced.
    let (idx, flags) = crate::clock::resolve_clock_source(source)?;
    let ticks = crate::time::read_clock(idx).ok_or(ArgumentError::InvalidArgument)?;
    let span = ticks.value * ticks.rate; // multiplication operator returns TimeSpan
    let precision = FemtoSeconds(1000); // TODO
    let resolution = ticks.rate;
    info_ptr.write(ClockInfo::new(
        span, precision, resolution, ticks.rate, flags,
    ));
    Ok(0)
}

fn type_get_random(into_ptr: u64, into_length: u64, flags: u64) -> Result<u64> {
    let flags: GetRandomFlags = flags.into();
    let into_ptr = unsafe { create_user_slice(into_ptr, into_length) }
        .ok_or(ArgumentError::InvalidArgument)?;
    let filled_buffer = getrandom(into_ptr, flags.contains(GetRandomFlags::NONBLOCKING));
    if !filled_buffer {
        Err(ResourceError::Unavailable.into())
    } else {
        // either it fills the entire length with entropy or it doesn't fill anything
        Ok(into_length)
    }
}

fn type_read_clock_list(
    clock: u64,
    clock_ptr: u64,
    slice_len: u64,
    start: u64,
    flags: u64,
) -> Result<u64> {
    // convert u64 back into things
    let slice = match unsafe { create_user_slice(clock_ptr, slice_len) } {
        Some(x) => x,
        None => return Err(ArgumentError::InvalidArgument.into()), // unknown error
    }; // maybe use ok or

    let kind: ClockKind = clock.into();

    let list_flags = match ReadClockListFlags::from_bits(flags as u32) {
        Some(x) => x,
        None => return Err(ArgumentError::InvalidArgument.into()), // invalid flag present
    };

    const EMPTY: ReadClockListFlags = ReadClockListFlags::empty();
    match list_flags {
        ReadClockListFlags::ALL_CLOCKS | EMPTY => fill_with_every_first(slice, start),
        ReadClockListFlags::ONLY_KIND => fill_with_kind(slice, kind, start),
        ReadClockListFlags::FIRST_KIND => fill_with_first_kind(slice, kind),
        _ => return Err(ArgumentError::InvalidArgument.into()), // invalid flag present
    }
    .map(|x| x as u64)
}

fn type_console_read(
    source: u64,
    buffer: u64,
    len: u64,
    flags: u64,
    timeout: u64,
    waiter: u64,
) -> Result<usize> {
    let timeout = unsafe { create_user_nullable_ptr(timeout) }
        .ok_or(ArgumentError::InvalidArgument)?
        .map(|t| *t);
    let waiter = unsafe { create_user_nullable_ptr(waiter) }
        .ok_or(ArgumentError::InvalidArgument)?
        .map(|w| *w);
    let flags = twizzler_abi::syscall::KernelConsoleReadFlags::from_bits_truncate(flags);
    let source: KernelConsoleSource = source.into();
    if let Some(slice) = unsafe { create_user_slice(buffer, len) } {
        match source {
            KernelConsoleSource::DebugConsole => {
                crate::log::read_bytes(source, slice, flags, timeout, waiter)
            }
            KernelConsoleSource::Console => {
                crate::log::read_bytes(source, slice, flags, timeout, waiter)
            }
            KernelConsoleSource::Buffer => crate::log::read_buffer_bytes(slice),
        }
    } else {
        Err(ArgumentError::InvalidArgument.into())
    }
}

#[inline]
fn convert_result_to_codes<T, E, F, G>(result: core::result::Result<T, E>, f: F, g: G) -> (u64, u64)
where
    F: Fn(T) -> (u64, u64),
    G: Fn(E) -> (u64, u64),
{
    match result {
        Ok(t) => f(t),
        Err(e) => g(e),
    }
}

#[inline]
fn one_err<E: Into<u64>>(e: E) -> (u64, u64) {
    (1, e.into())
}

#[inline]
fn zero_err<E: Into<u64>>(e: E) -> (u64, u64) {
    (0, e.into())
}

#[inline]
fn zero_ok<T: Into<u64>>(t: T) -> (u64, u64) {
    (0, t.into())
}

fn do_syscall_entry<T: SyscallContext + core::fmt::Debug>(context: &mut T) {
    if context.num() as u64 != Syscall::KernelConsoleWrite.num() {
        log::trace!(
            "sys {}: {:?}",
            crate::thread::current_thread_ref().unwrap().id(),
            Syscall::from(context.num())
        );
    }

    /*
    log!(
        ">{}:{}<",
        crate::thread::current_thread_ref().unwrap().id(),
        context.num()
    );
    */
    match context.num().into() {
        Syscall::ObjectUnmap => {
            let hi = context.arg0();
            let lo = context.arg1();
            let slot = context.arg2::<u64>() as usize;
            let handle = ObjID::from_parts([hi, lo]);
            let handle = if handle.raw() == 0 {
                None
            } else {
                Some(handle)
            };
            let result = object::sys_object_unmap(handle, slot);
            // No `scan_deleted()` here by default. It walks the *entire* global object map under
            // the global map lock, then re-takes that lock to remove what it found -- charged to
            // every unmap, to reap the at-most-one object this unmap could have made reapable.
            // Reaping is driven by the bsp idle loop instead (see `main`), and by
            // `ObjectControlCmd::Delete`, which still scans inline.
            //
            // A/B arm selector; see entryperf.md, §7 and "§8 under suspicion". `true` restores the
            // synchronous sweep, i.e. pre-§7 behaviour. The deferral is the one change in this tree
            // whose shape can plausibly produce a multi-millisecond constant -- reclaim that waits
            // on the bsp idle loop rather than happening inline -- which is why it is tested first
            // and the interrupt-mask rework is eliminated on arithmetic instead.
            const SCAN_ON_UNMAP: bool = false;
            if SCAN_ON_UNMAP {
                crate::obj::scan_deleted();
            }
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::Null => {
            if context.arg0::<u64>() == crate::perfmark::MAGIC {
                crate::perfmark::mark(context.arg1::<u64>() != 0);
                return;
            }
            if context.arg0::<u64>() == 0x12345678 {
                crate::thread::locktrack::diag::print_counters(true);
                crate::memory::pagetables::print_switch_counters();
                crate::memory::pagetables::print_shootdown_counters();
                crate::memory::context::virtmem::unmap_census::print();
                crate::memory::context::virtmem::slotmemo::print();
                sync::syncbatch::print();
                sync::requeuebug::print();
                crate::obj::pagetables::invl_overflow::print();
                crate::obj::pagetables::membership::print();
                // Before anything else: the machine is about to stop, and a queued background
                // sync that has not run is a guest's write discarded. The thread that would do it
                // runs at BACKGROUND priority with no claim on the time before poweroff.
                crate::pager::drain_background_sync();
                // Then tell the pager to write it all back. The drain only gets dirty pages as far
                // as the store's in-memory cache, which is write-back: without this the bytes are
                // complete in every in-memory sense and still absent from the disk.
                crate::pager::shutdown_pager();
                crate::obj::pagetables::tlbfix::print_stats();
                crate::memory::pagetables::nonleaf_cow_print();
                print_syscall_profile();
                crate::memory::context::virtmem::fault::print_fault_profile();
                crate::interrupt::print_interrupt_profile();
                crate::pager::print_pager_profile();
                crate::processor::sched::wakestats::print();
                crate::memory::context::virtmem::mapprofile::print();
                crate::memory::context::virtmem::unmapprofile::print();
                crate::memory::context::virtmem::heapprofile::print();
                crate::obj::id::checkidstats::print();
                object::mapstats::print();
                object::createprofile::print();
                crate::obj::coldfieldstats::print();
                crate::obj::reapstats::print();
                object::copystats::print();
                crate::memory::pagetables::zeroprobe::print();
                crate::arch::debug_shutdown(context.arg1::<u64>() as u32);
            }
            logln!(
                "{}: null call {:x} {:x} {:x} {:x} {:?}",
                current_thread_ref().unwrap().objid(),
                context.arg0::<u64>(),
                context.arg1::<u64>(),
                context.arg2::<u64>(),
                context.pc().raw(),
                context
            );
            crate::panic::backtrace(false, None);

            context.set_return_values(0u64, 0u64);
        }
        Syscall::KernelConsoleWrite => {
            let ptr = context.arg0();
            let len = context.arg1();
            let flags =
                twizzler_abi::syscall::KernelConsoleWriteFlags::from_bits_truncate(context.arg2());
            let target: KernelConsoleSource = context.arg3::<u64>().into();
            if let Some(slice) = unsafe { create_user_slice(ptr, len) } {
                sys_kernel_console_write(target, slice, flags);
            }
        }
        Syscall::KernelConsoleRead => {
            let res: Result<_> = type_console_read(
                context.arg0(),
                context.arg1(),
                context.arg2(),
                context.arg3(),
                context.arg4(),
                context.arg5(),
            )
            .map(|r| r as u64);
            let (code, val) = convert_result_to_codes(res, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::Kaction => {
            let cmd = context.arg0();
            let hi = context.arg1();
            let lo = context.arg2();
            let arg = context.arg3();
            let flags = context.arg4();
            let arg2 = context.arg5();
            let result = type_sys_kaction(cmd, hi, lo, arg, flags, arg2);
            let (code, val) = convert_result_to_codes(result, |v| v.into(), zero_err);
            context.set_return_values(code, val);
        }
        Syscall::NewHandle => {
            let hi = context.arg0();
            let lo = context.arg1();
            let handle_type = context.arg2::<u64>();
            let _flags = context.arg3::<u64>();
            let result = handle_type
                .try_into()
                .and_then(|nh: HandleType| sys_new_handle(ObjID::from_parts([hi, lo]), nh));
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::UnbindHandle => {
            let hi = context.arg0();
            let lo = context.arg1();
            let _flags = context.arg2::<u64>();
            let id = ObjID::from_parts([hi, lo]);
            sys_unbind_handle(id);
            context.set_return_values(0u64, 0u64);
        }
        Syscall::ObjectCreate => {
            let create = context.arg0();
            let src_ptr = context.arg1();
            let src_len = context.arg2();
            let tie_ptr = context.arg3();
            let tie_len = context.arg4();
            let result = type_sys_object_create(create, src_ptr, src_len, tie_ptr, tie_len);
            let (code, val) =
                convert_result_to_codes(result, |id| (id.parts()[0], id.parts()[1]), zero_err);
            context.set_return_values(code, val);
        }
        Syscall::ObjectCopy => {
            let id = ObjID::from_parts([context.arg0(), context.arg1()]);
            let result = type_sys_object_copy(id, context.arg2(), context.arg3());
            let (code, val) = convert_result_to_codes(result, |_| (0, 0), one_err);
            context.set_return_values(code, val);
        }
        Syscall::Spawn => {
            let args = context.arg0();
            let args = unsafe { create_user_ptr(args) };
            if let Some(args) = args {
                let result = thread::sys_spawn(args);
                let (code, val) =
                    convert_result_to_codes(result, |id| (id.parts()[0], id.parts()[1]), zero_err);
                context.set_return_values(code, val);
            } else {
                context
                    .set_return_values(0u64, TwzError::from(ArgumentError::InvalidArgument).raw());
            }
        }
        Syscall::ObjectMap => {
            let hi = context.arg0();
            let lo = context.arg1();
            let slot = context.arg2::<u64>() as usize;
            let prot = Protections::from_bits(context.arg3::<u64>() as u16);
            let flags = MapFlags::from_bits(context.arg4::<u64>() as u32);
            let id = ObjID::from_parts([hi, lo]);
            let margs = context.arg5();
            let margs = unsafe { create_user_ptr::<twizzler_abi::syscall::ObjectMapArgs>(margs) };
            let result = if let Some(margs) = margs {
                prot.map_or(Err(ArgumentError::InvalidArgument.into()), |prot| {
                    flags.map_or(Err(ArgumentError::InvalidArgument.into()), |flags| {
                        object::sys_object_map(
                            id,
                            slot,
                            prot,
                            margs.handle,
                            flags,
                            margs.target_sctx,
                        )
                    })
                })
                .map(|r| r as u64)
            } else {
                Err(ArgumentError::InvalidArgument.into())
            };
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ObjectReadMap => {
            let hi = context.arg0();
            let lo = context.arg1();
            let slot = context.arg2::<u64>() as usize;
            let id = ObjID::from_parts([hi, lo]);
            let out = context.arg3();
            let out = unsafe { create_user_ptr(out) };
            let result: Result<_> = if let Some(out) = out {
                object::sys_object_readmap(id, slot).map(|info| {
                    *out = info;
                    0u64
                })
            } else {
                Err(ArgumentError::InvalidArgument.into())
            };

            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::Ktrace => {
            let hi = context.arg0();
            let lo = context.arg1();
            let id = ObjID::from_parts([hi, lo]);
            let spec = context.arg2();
            let spec = unsafe { create_user_nullable_ptr(spec) };
            let result: Result<_> = if let Some(spec) = spec {
                crate::trace::sys::sys_ktrace(id, spec.map(|s| &*s))
            } else {
                Err(ArgumentError::InvalidArgument.into())
            };

            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::SctxAttach => {
            let hi = context.arg0();
            let lo = context.arg1();
            let id = ObjID::from_parts([hi, lo]);
            let result = sys_sctx_attach(id).map(|_| 0u64);
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ThreadSync => {
            let ptr = context.arg0();
            let len = context.arg1();
            let timeout = context.arg2();
            let result = type_sys_thread_sync(ptr, len, timeout);
            let (code, val) = convert_result_to_codes(result, |x| zero_ok(x as u64), one_err);
            context.set_return_values(code, val);
        }
        Syscall::SysInfo => {
            let ptr = context.arg0();
            let kind = context.arg1::<u64>();
            let info: Option<*mut u8> = unsafe { create_user_ptr(ptr).map(|r| r as *mut _) };
            if let Some(info) = info {
                let result = write_sysinfo(info, kind);
                let (code, val) = convert_result_to_codes(result, |_| (0u64, 0u64), one_err);
                context.set_return_values(code, val);
            } else {
                context.set_return_values(1u64, 1u64);
            }
        }
        Syscall::ThreadCtrl => {
            let target = ObjID::from_parts([context.arg0::<u64>(), context.arg1::<u64>()]);
            let [code, val] = thread_ctrl(
                context.arg2::<u64>().into(),
                if target.raw() == 0 {
                    None
                } else {
                    Some(target)
                },
                context.arg3(),
                context.arg4(),
                context.arg5(),
            );
            context.set_return_values(code, val);
            return;
        }
        Syscall::ObjectCtrl => {
            let id = ObjID::from_parts([context.arg0(), context.arg1()]);
            let cmd = (context.arg2::<u64>(), context.arg3::<u64>()).try_into();
            let result = cmd.and_then(|c| object_ctrl(id, c, context.arg4(), context.arg5()));
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
            return;
        }
        Syscall::MapCtrl => {
            let start = context.arg0::<u64>() as usize;
            let len = context.arg1::<u64>() as usize;
            let cmd = (context.arg2::<u64>(), context.arg3::<u64>()).try_into();
            let opts = context.arg4::<u64>();
            if let Ok(cmd) = cmd {
                let result = map_ctrl(start, len, cmd, opts);
                let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
                context.set_return_values(code, val);
            } else {
                context.set_return_values(1u64, 0u64);
            }
            return;
        }
        Syscall::ReadClockInfo => {
            let result = type_read_clock_info(context.arg0(), context.arg1(), context.arg2());
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::GetRandom => {
            let result = type_get_random(context.arg0(), context.arg1(), context.arg2());
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ReadClockList => {
            let result = type_read_clock_list(
                context.arg0(),
                context.arg1(),
                context.arg2(),
                context.arg3(),
                context.arg4(),
            );
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ObjectStat => {
            let hi = context.arg0();
            let lo = context.arg1();
            let id = ObjID::from_parts([hi, lo]);
            let out = context.arg2();
            let out: Option<&mut twizzler_abi::syscall::ObjectInfo> =
                unsafe { create_user_ptr(out) };
            let result: Result<_> = if let Some(out) = out {
                object::sys_object_info(id).map(|info| {
                    *out = info;
                    0u64
                })
            } else {
                Err(ArgumentError::InvalidArgument.into())
            };

            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::Enumerate => {
            let res = object::sys_enumerate(
                context.arg0(),
                context.arg1(),
                context.arg2(),
                context.arg3(),
            )
            .map(|r| r as u64);
            let (code, val) = convert_result_to_codes(res, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        _ => {
            context.set_return_values(1u64, 0u64);
        }
    }
}
/// Whether syscall durations are being collected.
///
/// The clock reads that produce them are not free, and a boot makes ~150,000 syscalls, so timing
/// every one of them to fill in statistics nobody has asked for is pure overhead. The test has to
/// be cheaper than what it skips: one relaxed load of a static that is written approximately never,
/// so it sits shared and clean in every cpu's L1.
///
/// Turned on by the first reader ([`get_syscall_stats`]) and by the syscall-exit trace point, so
/// the very first `SysInfo` after boot reports counts with empty timings and every later one is
/// populated.
static TIMING_ON: AtomicBool = AtomicBool::new(SYSCALL_PROFILE);

/// Collect the full per-syscall breakdown from boot -- timings on, `ThreadCtrl` split by command,
/// `ThreadSync` split by op kind, and every count attributed to both the calling security context
/// and the calling pc -- and dump it at `debug_shutdown`.
///
/// Off by default: it forces [`TIMING_ON`] and adds a lock-protected update per syscall, so the
/// timings it reports are inflated by its own cost. It answers "what is this workload asking the
/// kernel for, and from where", which is the question that found the address-space scan in
/// `bootstrap` -- 88% of every boot's syscalls.
pub const SYSCALL_PROFILE: bool = false;

#[inline]
fn timing_on() -> bool {
    TIMING_ON.load(Ordering::Relaxed)
}

/// Will anything read a [`SyscallEntryEvent`] if we build one? Checked before building it, not
/// after: the snapshot is ten loads, five spills and a `VirtAddr::new(pc).unwrap()` canonicality
/// test (with its panic edge), and it is discarded unread on every syscall of a boot that is not
/// being traced.
#[inline]
fn entry_snapshot_wanted() -> bool {
    SYSCALL_PROFILE
        || TRACE_MGR.any_enabled(TraceKind::Thread, THREAD_SYSCALL_ENTRY)
        || TRACE_MGR.any_enabled(TraceKind::Thread, THREAD_SYSCALL_EXIT)
}

pub fn syscall_entry<T: SyscallContext + core::fmt::Debug>(context: &mut T) {
    // The counters want only the number, so it is read unconditionally and the rest is not.
    let num: Syscall = context.num().into();
    // Taken here rather than rebuilt at exit because it cannot be rebuilt: `set_return_values`
    // writes the registers `num` and `arg2` are read back out of.
    let data = entry_snapshot_wanted().then(|| SyscallEntryEvent {
        ip: context.pc().raw(),
        num,
        args: [
            context.arg0(),
            context.arg1(),
            context.arg2(),
            context.arg3(),
            context.arg4(),
            context.arg5(),
        ],
    });
    if let Some(data) = data {
        trace_syscall_entry(data);
    }
    let start = timing_on().then(Instant::now);
    do_syscall_entry(context);
    let (r1, r2) = context.get_return_values();
    let duration = start.map(|start| (Instant::now() - start).into());
    add_syscall_stat_sample(num, data.as_ref(), duration);
    if let Some(data) = data {
        trace_syscall_exit(data, [r1, r2], duration);
    }
}

/// Per-syscall counts and timings.
///
/// Lives in [`crate::processor::Processor`], one per cpu. It used to be a single global behind a
/// `Spinlock`, taken and released on **every syscall** -- and a boot makes ~150,000 of them, so
/// every cpu in the system serialized on one lock and one cache line at every kernel exit. Nothing
/// about the data needs to be global: it is summed on the read path, which runs when someone asks
/// for `SysInfo`.
pub struct SyscallTracking {
    per_syscall_stats: [TimeStatCollector; Syscall::NumSyscalls as usize],
    prof: SyscallProfile,
}

/// The unconditional half of the per-cpu syscall accounting: monotonic counts, updated with
/// relaxed atomics so the exit path takes no lock and masks no interrupts for them. Everything
/// that needs mutual exclusion (the timing collectors, the gated profile) stays in
/// [`SyscallTracking`] behind its spinlock, which the exit path now touches only when timing is
/// actually on.
///
/// A preemption between resolving the current processor and the increment can land a count on the
/// cpu the thread just left; the read path sums across cpus, so that costs nothing. The locked
/// version tolerated the same by masking interrupts instead, which every syscall paid for.
pub struct SyscallCounts {
    pub total: AtomicUsize,
    pub per: [AtomicUsize; Syscall::NumSyscalls as usize],
}

impl SyscallCounts {
    pub fn new() -> Self {
        Self {
            total: AtomicUsize::new(0),
            per: core::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }
}

/// Instrumentation half of [`SyscallTracking`], live only under [`SYSCALL_PROFILE`].
pub struct SyscallProfile {
    /// `ThreadCtrl` by command number.
    thread_ctrl: [usize; NR_THREAD_CTRL],
    /// `ThreadSync` ops, by kind, summed over calls.
    sync_sleeps: usize,
    sync_wakes: usize,
    /// Calls carrying at least one sleep op, and the largest op array seen.
    sync_sleeping_calls: usize,
    sync_max_len: usize,
    /// Per-security-context attribution, i.e. per compartment.
    sctx: [(ObjID, [usize; Syscall::NumSyscalls as usize]); NR_SCTX_SLOTS],
    /// Hottest call sites (userspace pc) per syscall.
    sites: [[(u64, usize); NR_SITE_SLOTS]; Syscall::NumSyscalls as usize],
}

const NR_THREAD_CTRL: usize = 24;
const NR_SCTX_SLOTS: usize = 16;
const NR_SITE_SLOTS: usize = 6;

impl SyscallTracking {
    pub fn new() -> Self {
        Self {
            per_syscall_stats: core::array::from_fn(|_| TimeStatCollector::new()),
            prof: SyscallProfile {
                thread_ctrl: [0; NR_THREAD_CTRL],
                sync_sleeps: 0,
                sync_wakes: 0,
                sync_sleeping_calls: 0,
                sync_max_len: 0,
                sctx: core::array::from_fn(|_| (ObjID::new(0), [0; Syscall::NumSyscalls as usize])),
                sites: [[(0, 0); NR_SITE_SLOTS]; Syscall::NumSyscalls as usize],
            },
        }
    }
}

impl SyscallProfile {
    fn note(&mut self, entry: &SyscallEntryEvent, sctx: ObjID) {
        let syscall: Syscall = entry.num;
        match syscall {
            Syscall::ThreadCtrl => {
                let cmd = entry.args[2] as usize;
                if cmd < NR_THREAD_CTRL {
                    self.thread_ctrl[cmd] += 1;
                }
            }
            // ThreadSync's op kinds are counted by the sync path itself (`note_thread_sync_ops`),
            // which has the validated array; only the length is visible from here.
            Syscall::ThreadSync => {
                self.sync_max_len = self.sync_max_len.max(entry.args[1] as usize)
            }
            _ => {}
        }
        // First-come slots, so a caller that starts late can be missed; with six of them and this
        // few distinct call sites per syscall, in practice they are not.
        let sites = &mut self.sites[syscall as usize];
        if let Some(site) = sites.iter_mut().find(|s| s.0 == entry.ip || s.1 == 0) {
            site.0 = entry.ip;
            site.1 += 1;
        }

        for slot in self.sctx.iter_mut() {
            if slot.0 == sctx || slot.0.raw() == 0 {
                slot.0 = sctx;
                slot.1[syscall as usize] += 1;
                return;
            }
        }
    }
}

/// `entry` is present only when something asked for the full snapshot (see
/// [`entry_snapshot_wanted`]); the counts need the number alone, which is why it is passed
/// separately.
fn add_syscall_stat_sample(
    syscall: Syscall,
    entry: Option<&SyscallEntryEvent>,
    duration: Option<TimeSpan>,
) {
    // The unconditional counts are lock-free; see [`SyscallCounts`]. This used to take the per-cpu
    // spinlock -- an interrupt mask and a ticket acquisition -- on every syscall exit to bump them.
    let counts = &current_processor().syscall_counts;
    counts.total.fetch_add(1, Ordering::Relaxed);
    counts.per[syscall as usize].fetch_add(1, Ordering::Relaxed);
    // Per-thread, alongside the per-cpu totals above and on the same relaxed terms.
    if let Some(thread) = current_thread_ref() {
        thread.stats.syscalls.fetch_add(1, Ordering::Relaxed);
    }
    if duration.is_none() && !SYSCALL_PROFILE {
        return;
    }
    // Outside the lock: reading it takes one of its own, and it is only wanted for instrumentation.
    let sctx = if SYSCALL_PROFILE {
        current_thread_ref()
            .map(|t| t.active_sctx_id())
            .unwrap_or(ObjID::new(0))
    } else {
        ObjID::new(0)
    };
    // `Spinlock::lock` masks interrupts for the guard's lifetime, which is what makes this per-cpu
    // record uncontended by construction: only this cpu touches it, and it cannot be preempted off
    // it mid-update.
    let mut stats = current_processor().syscall_stats.lock();
    if let Some(duration) = duration {
        stats.per_syscall_stats[syscall as usize].add_sample(duration);
    }
    if SYSCALL_PROFILE {
        // `entry_snapshot_wanted` returns true whenever SYSCALL_PROFILE is set, so this is Some.
        if let Some(entry) = entry {
            stats.prof.note(entry, sctx);
        }
    }
}

/// Count one `sys_thread_sync` call's ops by kind. See [`SYSCALL_PROFILE`].
pub fn note_thread_sync_ops(ops: &[twizzler_abi::syscall::ThreadSync]) {
    if !SYSCALL_PROFILE {
        return;
    }
    let sleeps = ops
        .iter()
        .filter(|op| matches!(op, twizzler_abi::syscall::ThreadSync::Sleep(..)))
        .count();
    // See `add_syscall_stat_sample`: the lock already masks interrupts.
    let mut stats = current_processor().syscall_stats.lock();
    stats.prof.sync_sleeps += sleeps;
    stats.prof.sync_wakes += ops.len() - sleeps;
    if sleeps > 0 {
        stats.prof.sync_sleeping_calls += 1;
    }
}

/// Dump the [`SYSCALL_PROFILE`] breakdown, most-frequent first. Called from `debug_shutdown`.
pub fn print_syscall_profile() {
    if !SYSCALL_PROFILE {
        return;
    }
    let stats = get_syscall_stats();
    let mut order: alloc::vec::Vec<usize> = (0..Syscall::NumSyscalls as usize).collect();
    order.sort_unstable_by_key(|i| core::cmp::Reverse(stats.nr_syscalls_per_type[*i]));

    logln!("== syscall profile: {} total ==", stats.nr_syscalls);
    for i in order {
        let count = stats.nr_syscalls_per_type[i];
        if count == 0 {
            continue;
        }
        let t = &stats.syscall_times[i];
        logln!(
            "  {:>7} {:>4}permille  mean {:>7} ns  max {:>9} ns  total {:>8} us  {:?}",
            count,
            count * 1000 / stats.nr_syscalls.max(1),
            t.mean.as_nanos(),
            t.max.as_nanos(),
            (t.mean.as_nanos() as usize * count) / 1000,
            Syscall::from(i)
        );
    }

    let mut sites = [[(0u64, 0usize); NR_SITE_SLOTS]; Syscall::NumSyscalls as usize];
    let mut thread_ctrl = [0usize; NR_THREAD_CTRL];
    let (mut sleeps, mut wakes, mut sleeping_calls, mut max_len) = (0, 0, 0, 0);
    let mut sctx: alloc::vec::Vec<(ObjID, [usize; Syscall::NumSyscalls as usize])> =
        alloc::vec::Vec::new();
    with_each_active_processor(|p| {
        let stats = p.syscall_stats.lock();
        for i in 0..NR_THREAD_CTRL {
            thread_ctrl[i] += stats.prof.thread_ctrl[i];
        }
        sleeps += stats.prof.sync_sleeps;
        wakes += stats.prof.sync_wakes;
        sleeping_calls += stats.prof.sync_sleeping_calls;
        max_len = core::cmp::max(max_len, stats.prof.sync_max_len);
        for (s, per_cpu) in sites.iter_mut().zip(stats.prof.sites.iter()) {
            for (ip, count) in per_cpu.iter().filter(|s| s.1 > 0) {
                if let Some(slot) = s.iter_mut().find(|e| e.0 == *ip || e.1 == 0) {
                    slot.0 = *ip;
                    slot.1 += count;
                }
            }
        }
        for (id, counts) in stats.prof.sctx.iter() {
            if id.raw() == 0 {
                continue;
            }
            if let Some(e) = sctx.iter_mut().find(|e| e.0 == *id) {
                for i in 0..Syscall::NumSyscalls as usize {
                    e.1[i] += counts[i];
                }
            } else {
                sctx.push((*id, *counts));
            }
        }
    });

    logln!("== ThreadCtrl by command ==");
    let mut order: alloc::vec::Vec<usize> = (0..NR_THREAD_CTRL).collect();
    order.sort_unstable_by_key(|i| core::cmp::Reverse(thread_ctrl[*i]));
    for i in order {
        if thread_ctrl[i] == 0 {
            continue;
        }
        logln!("  {:>7}  cmd {}", thread_ctrl[i], i);
    }
    logln!(
        "== ThreadSync: {} calls, {} sleep ops, {} wake ops, {} calls with a sleep, max array {} ==",
        stats.nr_syscalls_per_type[Syscall::ThreadSync as usize],
        sleeps,
        wakes,
        sleeping_calls,
        max_len
    );

    logln!("== call sites (pc, and offset within its object) ==");
    for i in 0..Syscall::NumSyscalls as usize {
        if stats.nr_syscalls_per_type[i] == 0 {
            continue;
        }
        let mut slots = sites[i];
        slots.sort_unstable_by_key(|s| core::cmp::Reverse(s.1));
        for (ip, count) in slots.iter().filter(|s| s.1 > 0) {
            logln!(
                "  {:>7}  {:?} at {:x} (slot {} off {:x})",
                count,
                Syscall::from(i),
                ip,
                ip / twizzler_abi::object::MAX_SIZE as u64,
                ip % twizzler_abi::object::MAX_SIZE as u64
            );
        }
    }

    logln!("== syscalls by security context ==");
    sctx.sort_unstable_by_key(|e| core::cmp::Reverse(e.1.iter().sum::<usize>()));
    for (id, counts) in sctx {
        let total: usize = counts.iter().sum();
        logln!("  sctx {}: {} total", id, total);
        let mut order: alloc::vec::Vec<usize> = (0..Syscall::NumSyscalls as usize).collect();
        order.sort_unstable_by_key(|i| core::cmp::Reverse(counts[*i]));
        for i in order.into_iter().take(6) {
            if counts[i] == 0 {
                continue;
            }
            logln!("      {:>7}  {:?}", counts[i], Syscall::from(i));
        }
    }
}

/// Total syscalls executed since boot. The liveness signal for
/// [`crate::thread::check_system_hang`]: a running system makes thousands a second, and a wedged
/// one makes none.
pub fn nr_syscalls() -> usize {
    let mut count = 0;
    with_each_active_processor(|p| count += p.syscall_counts.total.load(Ordering::Relaxed));
    count
}

/// Per-syscall (count, total nanoseconds) summed over cpus, for [`crate::perfmark`] to difference.
/// A/B: restore the old behaviour in which taking a snapshot switched timing on, to measure what
/// the instrument was costing every number ever recorded with it.
pub const SNAPSHOT_ENABLES_TIMING: bool = false;

pub fn syscall_snapshot() -> [(usize, u64); Syscall::NumSyscalls as usize] {
    if SNAPSHOT_ENABLES_TIMING {
        TIMING_ON.store(true, Ordering::Relaxed);
    }
    // Deliberately does NOT set `TIMING_ON`, unlike `get_syscall_stats`. `crate::perfmark::mark`
    // calls this, every sysbench bench brackets its body with a mark, and `Mark::new` runs before
    // `b.iter()` -- so switching timing on here put two clock reads and a per-cpu lock update on
    // every syscall of every bench, including the first. The instrument enabled the overhead it
    // then reported. Counts are collected unconditionally and are what the marker mostly uses;
    // ask for times with `SYSCALL_PROFILE`.
    let mut out = [(0usize, 0u64); Syscall::NumSyscalls as usize];
    with_each_active_processor(|p| {
        let stats = p.syscall_stats.lock();
        for (i, slot) in out.iter_mut().enumerate() {
            slot.0 += p.syscall_counts.per[i].load(Ordering::Relaxed);
            slot.1 += (stats.per_syscall_stats[i].sum_femtos() / 1_000_000) as u64;
        }
    });
    out
}

fn get_syscall_stats() -> SyscallStats {
    // Asking for the stats is what turns their collection on; see `TIMING_ON`.
    TIMING_ON.store(true, Ordering::Relaxed);
    let mut merged: [TimeStatCollector; Syscall::NumSyscalls as usize] =
        core::array::from_fn(|_| TimeStatCollector::new());
    let mut syscall_stats = SyscallStats::default();

    with_each_active_processor(|p| {
        let stats = p.syscall_stats.lock();
        syscall_stats.nr_syscalls += p.syscall_counts.total.load(Ordering::Relaxed);
        for i in 0..Syscall::NumSyscalls as usize {
            syscall_stats.nr_syscalls_per_type[i] +=
                p.syscall_counts.per[i].load(Ordering::Relaxed);
            merged[i].merge(&stats.per_syscall_stats[i]);
        }
    });

    for (i, stat) in merged.iter().enumerate() {
        syscall_stats.syscall_times[i] = stat.get_stats();
    }

    syscall_stats
}

fn trace_syscall_entry(data: SyscallEntryEvent) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, THREAD_SYSCALL_ENTRY) {
        let entry = new_trace_entry(
            TraceKind::Thread,
            THREAD_SYSCALL_ENTRY,
            TraceEntryFlags::HAS_DATA,
        );

        TRACE_MGR.enqueue(TraceEvent::new_with_data(entry, data));
    }
}

fn trace_syscall_exit(entry: SyscallEntryEvent, ret: [u64; 2], duration: Option<TimeSpan>) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, THREAD_SYSCALL_EXIT) {
        // A sink appeared since the last exit, so nothing timed this one. Report it as zero and
        // switch timing on; the next syscall carries a real duration.
        TIMING_ON.store(true, Ordering::Relaxed);
        let data = SyscallExitEvent {
            entry,
            ret,
            duration: duration.unwrap_or_default(),
        };
        let entry = new_trace_entry(
            TraceKind::Thread,
            THREAD_SYSCALL_EXIT,
            TraceEntryFlags::HAS_DATA,
        );

        TRACE_MGR.enqueue(TraceEvent::new_with_data(entry, data));
    }
}
