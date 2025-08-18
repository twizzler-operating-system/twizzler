use core::mem::MaybeUninit;

use object::{map_ctrl, object_ctrl};
use twizzler_abi::{
    kso::{KactionCmd, KactionValue},
    object::{ObjID, Protections},
    syscall::{
        ClockFlags, ClockInfo, ClockKind, ClockSource, FemtoSeconds, GetRandomFlags, HandleType,
        KernelConsoleSource, MapFlags, ReadClockListFlags, SysInfo, Syscall,
    },
    trace::{SyscallEntryEvent, TraceEntryFlags, TraceKind, THREAD_SYSCALL_ENTRY},
};
use twizzler_rt_abi::{
    error::{ArgumentError, ResourceError, TwzError},
    Result,
};

use self::{
    object::{sys_new_handle, sys_sctx_attach, sys_unbind_handle},
    thread::thread_ctrl,
};
use crate::{
    clock::{fill_with_every_first, fill_with_first_kind, fill_with_kind},
    memory::VirtAddr,
    random::getrandom,
    time::TICK_SOURCES,
    trace::{
        mgr::{TraceEvent, TRACE_MGR},
        new_trace_entry,
    },
};

// TODO: move the handle stuff into its own file and make this private.
pub mod object;
/* TODO: move the requeue stuff into sched and make this private */
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
}

pub unsafe fn create_user_slice<'a, T>(ptr: u64, len: u64) -> Option<&'a mut [T]> {
    /* TODO: verify pointers */
    Some(core::slice::from_raw_parts_mut(ptr as *mut T, len as usize))
}

unsafe fn create_user_ptr<'a, T>(ptr: u64) -> Option<&'a mut T> {
    (ptr as *mut T).as_mut()
}

unsafe fn create_user_nullable_ptr<'a, T>(ptr: u64) -> Option<Option<&'a mut T>> {
    Some((ptr as *mut T).as_mut())
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

fn type_sys_thread_sync(ptr: u64, len: u64, timeoutptr: u64) -> Result<usize> {
    let slice = unsafe { create_user_slice(ptr, len) }.ok_or(ArgumentError::InvalidArgument)?;
    let timeout =
        unsafe { create_user_nullable_ptr(timeoutptr) }.ok_or(ArgumentError::InvalidArgument)?;
    sync::sys_thread_sync(slice, timeout)
}

fn write_sysinfo(info: &mut SysInfo) {
    info.cpu_count = crate::processor::all_processors().iter().fold(0, |acc, p| {
        acc + match &p {
            Some(p) => {
                if p.is_running() {
                    1
                } else {
                    0
                }
            }
            None => 0,
        }
    });
    info.flags = 0;
    info.version = 1;
    info.page_size = 0x1000;
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

    match source {
        ClockSource::BestMonotonic => {
            let ticks = { TICK_SOURCES.lock()[src as usize].read() };
            let span = ticks.value * ticks.rate; // multiplication operator returns TimeSpan
            let precision = FemtoSeconds(1000); // TODO
            let resolution = ticks.rate;
            let flags = ClockFlags::MONOTONIC;
            let info = ClockInfo::new(span, precision, resolution, flags);
            info_ptr.write(info);
            Ok(0)
        }
        ClockSource::BestRealTime => {
            let ticks = { TICK_SOURCES.lock()[src as usize].read() };
            let span = ticks.value * ticks.rate; // multiplication operator returns TimeSpan
            let precision = FemtoSeconds(1000); // TODO
            let resolution = ticks.rate;
            let flags = ClockFlags::empty();
            let info = ClockInfo::new(span, precision, resolution, flags);
            info_ptr.write(info);
            Ok(0)
        }
        ClockSource::ID(_) => {
            let ticks = {
                let clock_list = TICK_SOURCES.lock();
                if src as usize > clock_list.len() {
                    return Err(ArgumentError::InvalidArgument.into());
                }
                clock_list[src as usize].read()
            };
            let span = ticks.value * ticks.rate; // multiplication operator returns TimeSpan
            let precision = FemtoSeconds(1000); // TODO
            let resolution = ticks.rate;
            let flags = ClockFlags::empty();
            let info = ClockInfo::new(span, precision, resolution, flags);
            info_ptr.write(info);
            Ok(0)
        }
    }
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

pub fn syscall_entry<T: SyscallContext>(context: &mut T) {
    if context.num() as u64 != Syscall::KernelConsoleWrite.num() {
        log::trace!(
            "sys {}: {}",
            crate::thread::current_thread_ref().unwrap().id(),
            context.num()
        );
    }
    trace_syscall(
        context.pc(),
        context.num().into(),
        [
            context.arg0(),
            context.arg1(),
            context.arg2(),
            context.arg3(),
            context.arg4(),
            context.arg5(),
        ],
    );
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
            crate::obj::scan_deleted();
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
            context.set_return_values(1u64, 0u64);
        }
        Syscall::Null => {
            if context.arg0::<u64>() == 0x12345678 {
                crate::arch::debug_shutdown(context.arg1::<u64>() as u32);
            }
            logln!(
                "null call {:x} {:x} {:x}",
                context.arg0::<u64>(),
                context.arg1::<u64>(),
                context.arg2::<u64>(),
            );
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
            let handle = context.arg5();
            let handle = unsafe { create_user_ptr(handle) };
            let result = if let Some(handle) = handle {
                prot.map_or(Err(ArgumentError::InvalidArgument.into()), |prot| {
                    flags.map_or(Err(ArgumentError::InvalidArgument.into()), |flags| {
                        object::sys_object_map(id, slot, prot, *handle, flags)
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
            let info: Option<&mut SysInfo> = unsafe { create_user_ptr(ptr) };
            if let Some(info) = info {
                write_sysinfo(info);
                context.set_return_values(0u64, 0u64);
            } else {
                context.set_return_values(1u64, 0u64);
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
            );
            context.set_return_values(code, val);
            return;
        }
        Syscall::ObjectCtrl => {
            let id = ObjID::from_parts([context.arg0(), context.arg1()]);
            let cmd = (context.arg2::<u64>(), context.arg3::<u64>()).try_into();
            if let Ok(cmd) = cmd {
                let (code, val) = object_ctrl(id, cmd);
                context.set_return_values(code, val);
            } else {
                context.set_return_values(1u64, 0u64);
            }
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
        _ => {
            context.set_return_values(1u64, 0u64);
        }
    }
}

fn trace_syscall(ip: VirtAddr, num: Syscall, args: [u64; 6]) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, THREAD_SYSCALL_ENTRY) {
        let data = SyscallEntryEvent {
            ip: ip.raw(),
            num,
            args,
        };
        let entry = new_trace_entry(
            TraceKind::Thread,
            THREAD_SYSCALL_ENTRY,
            TraceEntryFlags::HAS_DATA,
        );

        TRACE_MGR.enqueue(TraceEvent::new_with_data(entry, data));
    }
}
