use core::{mem::MaybeUninit};

use twizzler_abi::{
    kso::{KactionCmd, KactionError, KactionValue},
    object::{ObjID, Protections},
    syscall::{
        ClockFlags, ReadClockListFlags, ClockInfo, ClockSource, ClockKind, FemtoSeconds, HandleType, KernelConsoleReadSource,
        ObjectCreateError, ObjectMapError, ReadClockInfoError, ReadClockListError, SysInfo, Syscall, ThreadSpawnError, ThreadSyncError,
    },
};
use x86_64::VirtAddr;

use crate::clock::{fill_with_every_first, fill_with_kind, fill_with_first_kind};
use crate::time::TICK_SOURCES;

use self::{object::sys_new_handle, thread::thread_ctrl};

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

unsafe fn create_user_slice<'a, T>(ptr: u64, len: u64) -> Option<&'a mut [T]> {
    /* TODO: verify pointers */
    Some(core::slice::from_raw_parts_mut(ptr as *mut T, len as usize))
}

unsafe fn create_user_ptr<'a, T>(ptr: u64) -> Option<&'a mut T> {
    (ptr as *mut T).as_mut()
}

unsafe fn create_user_nullable_ptr<'a, T>(ptr: u64) -> Option<Option<&'a mut T>> {
    Some((ptr as *mut T).as_mut())
}

fn sys_kernel_console_write(data: &[u8], flags: twizzler_abi::syscall::KernelConsoleWriteFlags) {
    let _res = crate::log::write_bytes(data, flags.into());
}

fn type_sys_object_create(
    create: u64,
    src_ptr: u64,
    src_len: u64,
    tie_ptr: u64,
    tie_len: u64,
) -> Result<ObjID, ObjectCreateError> {
    let srcs =
        unsafe { create_user_slice(src_ptr, src_len) }.ok_or(ObjectCreateError::InvalidArgument)?;
    let ties =
        unsafe { create_user_slice(tie_ptr, tie_len) }.ok_or(ObjectCreateError::InvalidArgument)?;
    let create = unsafe { create_user_ptr(create) }.ok_or(ObjectCreateError::InvalidArgument)?;
    object::sys_object_create(create, srcs, ties)
}

fn type_sys_thread_sync(ptr: u64, len: u64, timeoutptr: u64) -> Result<usize, ThreadSyncError> {
    let slice = unsafe { create_user_slice(ptr, len) }.ok_or(ThreadSyncError::InvalidArgument)?;
    let timeout =
        unsafe { create_user_nullable_ptr(timeoutptr) }.ok_or(ThreadSyncError::InvalidArgument)?;
    sync::sys_thread_sync(slice, timeout)
}

fn write_sysinfo(info: &mut SysInfo) {
    // TODO
    info.cpu_count = 1;
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
) -> Result<KactionValue, KactionError> {
    let cmd = KactionCmd::try_from(cmd)?;
    let objid = if hi == 0 {
        None
    } else {
        Some(ObjID::new_from_parts(hi, lo))
    };
    crate::device::kaction(cmd, objid, arg)
}

fn type_read_clock_info(src: u64, info: u64, _flags: u64) -> Result<u64, ReadClockInfoError> {
    let source: ClockSource = src.into();
    let info_ptr: &mut MaybeUninit<ClockInfo> =
        unsafe { create_user_ptr(info) }.ok_or(ReadClockInfoError::InvalidArgument)?;

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
        _ => Err(ReadClockInfoError::InvalidArgument),
    }
}

fn type_read_clock_list(
    clock: u64,
    clock_ptr: u64,
    slice_len: u64,
    start: u64,
    flags: u64
) -> Result<u64, ReadClockListError> {
    // convert u64 back into things
    let slice = match unsafe { create_user_slice(clock_ptr, slice_len) } {
        Some(x) => x,
        None => return Err(ReadClockListError::Unknown) // unknown error
    }; // maybe use ok or

    let kind: ClockKind = clock.into();

    let list_flags = match ReadClockListFlags::from_bits(flags as u32) {
        Some(x) => x,
        None => return Err(ReadClockListError::InvalidArgument) // invalid flag present
    };

    const EMPTY: ReadClockListFlags = ReadClockListFlags::empty();
    match list_flags {
        ReadClockListFlags::ALL_CLOCKS | EMPTY => fill_with_every_first(slice, start),
        ReadClockListFlags::ONLY_KIND => fill_with_kind(slice, kind, start),
        ReadClockListFlags::FIRST_KIND => fill_with_first_kind(slice, kind),
        _ => Err(ReadClockListError::InvalidArgument) // invalid flag combination
    }
    .map(|x| x as u64)
}

#[inline]
fn convert_result_to_codes<T, E, F, G>(result: Result<T, E>, f: F, g: G) -> (u64, u64)
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
    /*
    logln!(
        "syscall! {} {}",
        crate::thread::current_thread_ref().unwrap().id(),
        context.num()
    );
    */
    match context.num().into() {
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
            if let Some(slice) = unsafe { create_user_slice(ptr, len) } {
                sys_kernel_console_write(slice, flags);
            }
        }
        Syscall::KernelConsoleRead => {
            let source = context.arg0::<u64>();
            let ptr = context.arg1();
            let len = context.arg2();
            let source: KernelConsoleReadSource = source.into();
            let res = if let Some(slice) = unsafe { create_user_slice(ptr, len) } {
                match source {
                    KernelConsoleReadSource::Console => {
                        let flags =
                            twizzler_abi::syscall::KernelConsoleReadFlags::from_bits_truncate(
                                context.arg2(),
                            );
                        crate::log::read_bytes(slice, flags).map_err(|x| x.into())
                    }
                    KernelConsoleReadSource::Buffer => {
                        let _flags =
                            twizzler_abi::syscall::KernelConsoleReadBufferFlags::from_bits_truncate(
                                context.arg2(),
                            );
                        crate::log::read_buffer_bytes(slice).map_err(|x| x.into())
                    }
                }
            } else {
                Err(0u64)
            }
            .map(|x| x as u64);
            let (code, val) = convert_result_to_codes(res, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::Kaction => {
            let cmd = context.arg0();
            let hi = context.arg1();
            let lo = context.arg2();
            let arg = context.arg3();
            let flags = context.arg4();
            let result = type_sys_kaction(cmd, hi, lo, arg, flags);
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
                .and_then(|nh: HandleType| sys_new_handle(ObjID::new_from_parts(hi, lo), nh));
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ObjectCreate => {
            let create = context.arg0();
            let src_ptr = context.arg1();
            let src_len = context.arg2();
            let tie_ptr = context.arg3();
            let tie_len = context.arg4();
            let result = type_sys_object_create(create, src_ptr, src_len, tie_ptr, tie_len);
            let (code, val) = convert_result_to_codes(result, |id| id.split(), zero_err);
            context.set_return_values(code, val);
        }
        Syscall::Spawn => {
            let args = context.arg0();
            let args = unsafe { create_user_ptr(args) };
            if let Some(args) = args {
                let result = thread::sys_spawn(args);
                let (code, val) = convert_result_to_codes(result, |id| id.split(), zero_err);
                context.set_return_values(code, val);
            } else {
                context.set_return_values(0u64, ThreadSpawnError::InvalidArgument as u64);
            }
        }
        Syscall::ObjectMap => {
            let hi = context.arg0();
            let lo = context.arg1();
            let slot = context.arg2::<u64>() as usize;
            let prot = Protections::from_bits(context.arg3::<u64>() as u32);
            let id = ObjID::new_from_parts(hi, lo);
            let handle = context.arg5();
            let handle = unsafe { create_user_ptr(handle) };
            let result = if let Some(handle) = handle {
                prot.map_or(Err(ObjectMapError::InvalidProtections), |prot| {
                    object::sys_object_map(id, slot, prot, *handle)
                })
                .map(|r| r as u64)
            } else {
                Err(ObjectMapError::InvalidArgument)
            };
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
            let (code, val) = thread_ctrl(context.arg0::<u64>().into(), context.arg1());
            context.set_return_values(code, val);
        }
        Syscall::ReadClockInfo => {
            let result = type_read_clock_info(context.arg0(), context.arg1(), context.arg2());
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ReadClockList => {
            let result = type_read_clock_list(
                context.arg0(), context.arg1(), context.arg2(), context.arg3(), context.arg4());
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        _ => {
            context.set_return_values(1u64, 0u64);
        }
    }
}
