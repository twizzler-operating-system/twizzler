use twizzler_abi::{
    object::{objid_from_parts, Protections},
    syscall::{
        ObjectCreateError, ObjectMapError, SysInfo, Syscall, ThreadSpawnError, ThreadSyncError,
    },
};
use x86_64::VirtAddr;

use twizzler_abi::object::ObjID;

use self::thread::thread_ctrl;

mod object;
mod sync;
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

fn type_sys_thread_sync(ptr: u64, len: u64, timeoutptr: u64) -> Result<u64, ThreadSyncError> {
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
    //logln!("syscall! {}", context.num());
    match context.num().into() {
        Syscall::KernelConsoleWrite => {
            let ptr = context.arg0();
            let len = context.arg1();
            let flags =
                twizzler_abi::syscall::KernelConsoleWriteFlags::from_bits_truncate(context.arg2());
            if let Some(slice) = unsafe { create_user_slice(ptr, len) } {
                sys_kernel_console_write(slice, flags);
            }
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
            let id = objid_from_parts(hi, lo);
            let result = prot
                .map_or(Err(ObjectMapError::InvalidProtections), |prot| {
                    object::sys_object_map(id, slot, prot)
                })
                .map(|r| r as u64);
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
            context.set_return_values(code, val);
        }
        Syscall::ThreadSync => {
            let ptr = context.arg0();
            let len = context.arg1();
            let timeout = context.arg2();
            let result = type_sys_thread_sync(ptr, len, timeout);
            let (code, val) = convert_result_to_codes(result, zero_ok, one_err);
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
        _ => {
            context.set_return_values(1u64, 0u64);
        }
    }
}
