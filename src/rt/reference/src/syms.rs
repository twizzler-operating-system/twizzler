#![allow(unused_variables)]
#![allow(non_snake_case)]
#![allow(improper_ctypes_definitions)]

// This macro checks that our definition of a function is the same as that
// defined by the bindings generated from bindgen. Thus the whole ABI
// is type-checked! The only trick is that you have to specify the number of arguments.
macro_rules! check_ffi_type {
    ($f1:ident) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn() -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_, _) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _, _, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_, _, _) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _, _, _, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_, _, _, _) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _, _, _, _, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_, _, _, _, _) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _, _, _, _, _, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_, _, _, _, _, _) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
    ($f1:ident, _, _, _, _, _, _, _) => {
        paste::paste! {
            #[allow(dead_code, unused_variables, unused_assignments)]
            fn [<__tc_ffi_ $f1>]() {
                let mut x: unsafe extern "C-unwind" fn(_, _, _, _, _, _, _) -> _ = $f1;
                x = twizzler_rt_abi::bindings::$f1;
            }
        }
    };
}

use std::{
    alloc::GlobalAlloc,
    ffi::{c_void, CStr},
};

use tracing::warn;
use twizzler_abi::{
    object::{ObjID, MAX_SIZE},
    syscall::ObjectCreate,
};
// core.h
use twizzler_rt_abi::bindings::{
    binding_info, endpoint, fd_set, io_ctx, name_resolver, name_root, object_cmd, object_create,
    object_source, object_tie, option_exit_code, release_flags, twz_error, u32_result, wait_kind,
};
use twizzler_rt_abi::error::{ArgumentError, RawTwzError, TwzError};

use crate::{runtime::OUR_RUNTIME, set_upcall_handler};

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_abort() {
    OUR_RUNTIME.abort();
}
check_ffi_type!(twz_rt_abort);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_exit(code: i32) {
    OUR_RUNTIME.exit(code);
}
check_ffi_type!(twz_rt_exit, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_pre_main_hook() -> option_exit_code {
    match OUR_RUNTIME.pre_main_hook() {
        Some(ec) => option_exit_code {
            is_some: 1,
            value: ec,
        },
        None => option_exit_code {
            is_some: 0,
            value: 0,
        },
    }
}
check_ffi_type!(twz_rt_pre_main_hook);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_post_main_hook() {
    OUR_RUNTIME.post_main_hook()
}
check_ffi_type!(twz_rt_post_main_hook);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_runtime_entry(
    arg: *const twizzler_rt_abi::bindings::runtime_info,
    std_entry: core::option::Option<
        unsafe extern "C-unwind" fn(
            arg1: twizzler_rt_abi::bindings::basic_aux,
        ) -> twizzler_rt_abi::bindings::basic_return,
    >,
    main: usize,
) {
    OUR_RUNTIME.runtime_entry(arg, std_entry.unwrap_unchecked(), main)
}
check_ffi_type!(twz_rt_runtime_entry, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_cross_compartment_entry() -> bool {
    OUR_RUNTIME.cross_compartment_entry().is_ok()
}
check_ffi_type!(twz_rt_cross_compartment_entry);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_set_upcall_handler(
    handler: Option<unsafe extern "C-unwind" fn(frame: *mut c_void, data: *const c_void)>,
) {
    let _ = set_upcall_handler(handler);
}
check_ffi_type!(twz_rt_set_upcall_handler, _);

// alloc.h

use twizzler_rt_abi::bindings::{alloc_flags, ZERO_MEMORY};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_malloc(
    sz: usize,
    align: usize,
    flags: alloc_flags,
) -> *mut ::core::ffi::c_void {
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, align) else {
        return core::ptr::null_mut();
    };
    if flags & ZERO_MEMORY != 0 {
        OUR_RUNTIME.alloc_zeroed(layout).cast()
    } else {
        OUR_RUNTIME.alloc(layout).cast()
    }
}
check_ffi_type!(twz_rt_malloc, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_dealloc(
    ptr: *mut ::core::ffi::c_void,
    sz: usize,
    align: usize,
    flags: twizzler_rt_abi::bindings::alloc_flags,
) {
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, align) else {
        return;
    };
    if flags & ZERO_MEMORY != 0 {
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u8>(), sz) };
        slice.fill(0);
        core::hint::black_box(slice);
    }
    OUR_RUNTIME.dealloc(ptr.cast(), layout);
}
check_ffi_type!(twz_rt_dealloc, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_realloc(
    ptr: *mut ::core::ffi::c_void,
    sz: usize,
    align: usize,
    new_size: usize,
    flags: twizzler_rt_abi::bindings::alloc_flags,
) -> *mut ::core::ffi::c_void {
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, align) else {
        return core::ptr::null_mut();
    };
    if flags & ZERO_MEMORY != 0 {
        todo!()
    }
    OUR_RUNTIME.realloc(ptr.cast(), layout, new_size).cast()
}
check_ffi_type!(twz_rt_realloc, _, _, _, _, _);

// thread.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wait(
    ptr: *mut u32,
    expected: twizzler_rt_abi::bindings::futex_word,
    timeout: twizzler_rt_abi::bindings::option_duration,
) -> twizzler_rt_abi::bindings::twz_error {
    if timeout.is_some != 0 {
        OUR_RUNTIME.futex_wait(&*ptr.cast(), expected, Some(timeout.dur.into()))
    } else {
        OUR_RUNTIME.futex_wait(&*ptr.cast(), expected, None)
    }
}
check_ffi_type!(twz_rt_futex_wait, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wake(
    ptr: *mut u32,
    max: i64,
) -> twizzler_rt_abi::bindings::twz_error {
    OUR_RUNTIME.futex_wake(&*ptr.cast(), max as usize)
}
check_ffi_type!(twz_rt_futex_wake, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_yield_now() {
    OUR_RUNTIME.yield_now();
}
check_ffi_type!(twz_rt_yield_now);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_set_name(name: *const ::core::ffi::c_char) {
    unsafe {
        OUR_RUNTIME.set_name(core::ffi::CStr::from_ptr(name));
    }
}
check_ffi_type!(twz_rt_set_name, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_name(
    tcb: *const c_void,
    name: *mut core::ffi::c_char,
    len: *mut usize,
) {
    unsafe {
        *len = OUR_RUNTIME.get_name(tcb, core::slice::from_raw_parts_mut(name.cast(), *len));
    }
}
check_ffi_type!(twz_rt_get_name, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_sleep(dur: twizzler_rt_abi::bindings::duration) {
    OUR_RUNTIME.sleep(dur.into());
}
check_ffi_type!(twz_rt_sleep, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_tls_get_addr(
    index: *mut twizzler_rt_abi::bindings::tls_index,
) -> *mut ::core::ffi::c_void {
    OUR_RUNTIME
        .tls_get_addr(unsafe { &*index })
        .unwrap_or(core::ptr::null_mut())
        .cast()
}
check_ffi_type!(twz_rt_tls_get_addr, _);

// Provide this for C, since this will be emitted by the C compiler.
#[no_mangle]
pub unsafe extern "C-unwind" fn __tls_get_addr(
    index: *mut twizzler_rt_abi::bindings::tls_index,
) -> *mut ::core::ffi::c_void {
    twz_rt_tls_get_addr(index)
}

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_spawn_thread(
    args: twizzler_rt_abi::bindings::spawn_args,
) -> twizzler_rt_abi::bindings::spawn_result {
    OUR_RUNTIME.spawn(args).into()
}
check_ffi_type!(twz_rt_spawn_thread, _);
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_join_thread(
    id: twizzler_rt_abi::bindings::thread_id,
    timeout: twizzler_rt_abi::bindings::option_duration,
) -> twizzler_rt_abi::bindings::twz_error {
    match if timeout.is_some != 0 {
        OUR_RUNTIME.join(id, Some(timeout.dur.into()))
    } else {
        OUR_RUNTIME.join(id, None)
    } {
        Ok(_) => RawTwzError::success().raw(),
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_join_thread, _, _);

// fd.h

use twizzler_rt_abi::bindings::{descriptor, open_kind, open_result};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_open(
    kind: open_kind,
    flags: u32,
    bind_info: *mut c_void,
    bind_info_len: usize,
) -> open_result {
    let Ok(kind) = kind.try_into() else {
        return Err(ArgumentError::InvalidArgument.into()).into();
    };
    OUR_RUNTIME
        .open(None, kind, flags.into(), bind_info, bind_info_len, true)
        .into()
}
check_ffi_type!(twz_rt_fd_open, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_reopen(
    fd: descriptor,
    kind: open_kind,
    flags: u32,
    bind_info: *mut c_void,
    bind_info_len: usize,
) -> twz_error {
    let Ok(kind) = kind.try_into() else {
        return TwzError::INVALID_ARGUMENT.raw();
    };
    match OUR_RUNTIME.open(Some(fd), kind, flags.into(), bind_info, bind_info_len, true) {
        Ok(_) => RawTwzError::success().raw(),
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_fd_reopen, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_read_binds(
    binds: *mut binding_info,
    len: usize,
) -> usize {
    let binds = unsafe { core::slice::from_raw_parts_mut(binds, len) };
    OUR_RUNTIME.read_binds(binds)
}
check_ffi_type!(twz_rt_fd_read_binds, _, _);

use core::ffi::c_char;

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_remove(name: *const c_char, len: usize) -> twz_error {
    let name = unsafe { core::slice::from_raw_parts(name.cast(), len) };
    let name = core::str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT.raw());
    match name {
        Ok(name) => match OUR_RUNTIME.remove(name) {
            Ok(_) => RawTwzError::success().raw(),
            Err(e) => e.raw(),
        },
        Err(e) => e,
    }
}
check_ffi_type!(twz_rt_fd_remove, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_close(fd: descriptor) {
    OUR_RUNTIME.close(fd);
}
check_ffi_type!(twz_rt_fd_close, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_get_info(
    fd: descriptor,
    fd_info: *mut twizzler_rt_abi::bindings::fd_info,
) -> bool {
    match OUR_RUNTIME.fd_get_info(fd) {
        Some(info) => {
            fd_info.write(info);
            true
        }
        None => false,
    }
}
check_ffi_type!(twz_rt_fd_get_info, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_cmd(
    fd: descriptor,
    cmd: twizzler_rt_abi::bindings::fd_cmd,
    arg: *mut ::core::ffi::c_void,
    ret: *mut ::core::ffi::c_void,
) -> twz_error {
    match OUR_RUNTIME.fd_cmd(fd, cmd, arg.cast(), ret.cast()) {
        Ok(_) => RawTwzError::success().raw(),
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_fd_cmd, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_waitpoint(
    fd: descriptor,
    kind: wait_kind,
    point: *mut *mut u64,
    val: *mut u64,
) -> twz_error {
    match OUR_RUNTIME.fd_waitpoint(fd, kind) {
        Ok(ts) => {
            point.write(
                ts.reference
                    .address()
                    .unwrap_or(core::ptr::null_mut())
                    .cast(),
            );
            val.write(ts.value);
            RawTwzError::success().raw()
        }
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_fd_waitpoint, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_select(
    nfds: usize,
    readfds: *mut fd_set,
    writefds: *mut fd_set,
    exceptfds: *mut fd_set,
    timeout: twizzler_rt_abi::bindings::option_duration,
) -> io_result {
    match OUR_RUNTIME.select(
        nfds,
        readfds,
        writefds,
        exceptfds,
        if timeout.is_some != 0 {
            Some(timeout.dur.into())
        } else {
            None
        },
    ) {
        Ok(result) => io_result {
            err: TwzError::SUCCESS.raw(),
            val: result,
        },
        Err(e) => io_result {
            err: e.raw(),
            val: 0,
        },
    }
}
check_ffi_type!(twz_rt_fd_select, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_enumerate_names(
    fd: descriptor,
    buf: *mut twizzler_rt_abi::bindings::name_entry,
    len: ::core::ffi::c_size_t,
    off: ::core::ffi::c_size_t,
) -> io_result {
    OUR_RUNTIME
        .fd_enumerate(
            fd,
            unsafe { core::slice::from_raw_parts_mut(buf, len) },
            off,
        )
        .into()
}
check_ffi_type!(twz_rt_fd_enumerate_names, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_mkns(name: *const c_char, len: usize) -> twz_error {
    let name = unsafe { core::slice::from_raw_parts(name.cast(), len) };
    let name = core::str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT.raw());
    match name {
        Ok(name) => match OUR_RUNTIME.mkns(name) {
            Ok(_) => RawTwzError::success().raw(),
            Err(e) => e.raw(),
        },
        Err(e) => e,
    }
}
check_ffi_type!(twz_rt_fd_mkns, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_symlink(
    name: *const c_char,
    len: usize,
    target: *const c_char,
    target_len: usize,
) -> twz_error {
    let name = unsafe { core::slice::from_raw_parts(name.cast(), len) };
    let name = core::str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT.raw());
    let target = unsafe { core::slice::from_raw_parts(target.cast(), target_len) };
    let Ok(target) = core::str::from_utf8(target).map_err(|_| TwzError::INVALID_ARGUMENT.raw())
    else {
        return TwzError::INVALID_ARGUMENT.into();
    };
    match name {
        Ok(name) => match OUR_RUNTIME.symlink(name, target) {
            Ok(_) => RawTwzError::success().raw(),
            Err(e) => e.raw(),
        },
        Err(e) => e,
    }
}
check_ffi_type!(twz_rt_fd_symlink, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_thread_info(
    id: twizzler_rt_abi::bindings::thread_id,
) -> twizzler_rt_abi::bindings::thread_info {
    let id = if id == twizzler_rt_abi::bindings::TWZ_RT_THREAD_ID_SELF {
        None
    } else {
        Some(id)
    };
    OUR_RUNTIME.thread_get_info(id)
}
check_ffi_type!(twz_rt_get_thread_info, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_rename(
    old_name: *const c_char,
    old_len: usize,
    new_name: *const c_char,
    new_len: usize,
) -> twz_error {
    let old = unsafe { core::slice::from_raw_parts(old_name.cast(), old_len) };
    let old = core::str::from_utf8(old).map_err(|_| TwzError::INVALID_ARGUMENT.raw());
    let new = unsafe { core::slice::from_raw_parts(new_name.cast(), new_len) };
    let Ok(new) = core::str::from_utf8(new).map_err(|_| TwzError::INVALID_ARGUMENT.raw()) else {
        return TwzError::INVALID_ARGUMENT.into();
    };
    match old {
        Ok(old) => match OUR_RUNTIME.rename(old, new) {
            Ok(_) => RawTwzError::success().raw(),
            Err(e) => e.raw(),
        },
        Err(e) => e,
    }
}
check_ffi_type!(twz_rt_fd_rename, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_readlink(
    name: *const c_char,
    len: usize,
    target: *mut c_char,
    target_len: usize,
    read_len: *mut u64,
) -> twz_error {
    let name = unsafe { core::slice::from_raw_parts(name.cast(), len) };
    let name = core::str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT.raw());
    let target = unsafe { core::slice::from_raw_parts_mut(target.cast(), target_len) };
    match name {
        Ok(name) => match OUR_RUNTIME.readlink(name, target, unsafe { read_len.as_mut().unwrap() })
        {
            Ok(_) => RawTwzError::success().raw(),
            Err(e) => e.raw(),
        },
        Err(e) => e,
    }
}
check_ffi_type!(twz_rt_fd_readlink, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_nameroot(
    root: name_root,
    path: *mut c_char,
    len: usize,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts_mut(path.cast::<u8>(), len) };
    OUR_RUNTIME.get_nameroot(root.into(), slice).into()
}
check_ffi_type!(twz_rt_get_nameroot, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_set_nameroot(
    root: name_root,
    path: *const c_char,
    len: usize,
) -> twz_error {
    let slice = unsafe { core::slice::from_raw_parts(path.cast::<u8>(), len) };
    match OUR_RUNTIME.set_nameroot(root.into(), slice) {
        Ok(_) => RawTwzError::success().raw(),
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_set_nameroot, _, _, _);

// io.h
use twizzler_rt_abi::bindings::{io_result, iovec, whence};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pread(
    fd: descriptor,
    buf: *mut ::core::ffi::c_void,
    len: usize,
    ctx: *mut io_ctx,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    OUR_RUNTIME.fd_pread(fd, slice, ctx).into()
}
check_ffi_type!(twz_rt_fd_pread, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pwrite(
    fd: descriptor,
    buf: *const ::core::ffi::c_void,
    len: usize,
    ctx: *mut io_ctx,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts(buf.cast::<u8>(), len) };
    OUR_RUNTIME.fd_pwrite(fd, slice, ctx).into()
}
check_ffi_type!(twz_rt_fd_pwrite, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pwrite_to(
    fd: descriptor,
    buf: *const ::core::ffi::c_void,
    len: usize,
    ctx: *mut io_ctx,
    ep: *const endpoint,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts(buf.cast::<u8>(), len) };
    OUR_RUNTIME.fd_pwrite_to(fd, slice, ctx, ep).into()
}
check_ffi_type!(twz_rt_fd_pwrite_to, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pread_from(
    fd: descriptor,
    buf: *mut ::core::ffi::c_void,
    len: usize,
    ctx: *mut io_ctx,
    ep: *mut endpoint,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    OUR_RUNTIME.fd_pread_from(fd, slice, ctx, ep).into()
}
check_ffi_type!(twz_rt_fd_pread_from, _, _, _, _, _);

use twizzler_rt_abi::io::SeekFrom;

fn twz_sf_to_std_sf(sf: SeekFrom) -> std::io::SeekFrom {
    match sf {
        SeekFrom::Start(pos) => std::io::SeekFrom::Start(pos),
        SeekFrom::End(pos) => std::io::SeekFrom::End(pos),
        SeekFrom::Current(pos) => std::io::SeekFrom::Current(pos),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_seek(
    fd: descriptor,
    whence: whence,
    offset: i64,
) -> io_result {
    let seek = match whence {
        twizzler_rt_abi::bindings::WHENCE_START => SeekFrom::Start(offset as u64),
        twizzler_rt_abi::bindings::WHENCE_END => SeekFrom::End(offset),
        twizzler_rt_abi::bindings::WHENCE_CURRENT => SeekFrom::Current(offset),
        _ => {
            return io_result {
                val: 0,
                err: TwzError::INVALID_ARGUMENT.raw(),
            }
        }
    };
    OUR_RUNTIME.seek(fd, twz_sf_to_std_sf(seek)).into()
}
check_ffi_type!(twz_rt_fd_seek, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_preadv(
    fd: descriptor,
    iovs: *const iovec,
    nr_iovs: usize,
    ctx: *mut io_ctx,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts(iovs, nr_iovs) };
    OUR_RUNTIME.fd_preadv(fd, slice, ctx).into()
}
check_ffi_type!(twz_rt_fd_preadv, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pwritev(
    fd: descriptor,
    iovs: *const iovec,
    nr_iovs: usize,
    ctx: *mut io_ctx,
) -> io_result {
    let slice = unsafe { core::slice::from_raw_parts(iovs, nr_iovs) };
    OUR_RUNTIME.fd_pwritev(fd, slice, ctx).into()
}
check_ffi_type!(twz_rt_fd_pwritev, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_get_config(
    fd: descriptor,
    reg: u32,
    val: *mut c_void,
    val_len: usize,
) -> twz_error {
    match OUR_RUNTIME.fd_get_config(fd, reg, val, val_len) {
        Ok(_) => RawTwzError::success().raw(),
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_fd_get_config, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_set_config(
    fd: descriptor,
    reg: u32,
    val: *const c_void,
    val_len: usize,
) -> twz_error {
    match OUR_RUNTIME.fd_set_config(fd, reg, val, val_len) {
        Ok(_) => RawTwzError::success().raw(),
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_fd_set_config, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_resolve_name(
    resolver: name_resolver,
    name: *const c_char,
    name_len: usize,
) -> objid_result {
    let slice = unsafe { core::slice::from_raw_parts(name as *const u8, name_len) };
    result_id_to_bindings(OUR_RUNTIME.resolve_name(resolver.into(), slice))
}
check_ffi_type!(twz_rt_resolve_name, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_canon_name(
    resolver: name_resolver,
    name: *const c_char,
    name_len: usize,
    out: *mut c_char,
    out_len: *mut usize,
) -> twz_error {
    let slice = unsafe { core::slice::from_raw_parts(name as *const u8, name_len) };
    let out_slice = unsafe { core::slice::from_raw_parts_mut(out as *mut u8, out_len.read()) };
    match OUR_RUNTIME.canon_name(resolver.into(), slice, out_slice) {
        Ok(len) => {
            out_len.write(len);
            RawTwzError::success().raw()
        }
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_canon_name, _, _, _, _, _);

// object.h

fn result_id_to_bindings(value: Result<ObjID, TwzError>) -> objid_result {
    match value {
        Ok(id) => objid_result {
            err: RawTwzError::success().raw(),
            __bindgen_padding_0: 0,
            val: id.raw(),
        },
        Err(err) => objid_result {
            err: err.raw(),
            __bindgen_padding_0: 0,
            val: 0,
        },
    }
}

use twizzler_rt_abi::{
    bindings::{map_flags, map_result, object_handle, objid, objid_result},
    object::MapFlags,
};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_create_rtobj() -> objid_result {
    result_id_to_bindings(OUR_RUNTIME.create_rtobj())
}
check_ffi_type!(twz_rt_create_rtobj);

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn twz_rt_create_object(
    spec: *const object_create,
    src: *const object_source,
    src_len: usize,
    ties: *const object_tie,
    tie_len: usize,
    name: *const c_char,
    namelen: usize,
) -> objid_result {
    let spec = spec.as_ref().unwrap();
    let src = if !src.is_null() {
        core::slice::from_raw_parts(src, src_len)
    } else {
        &[]
    };
    let ties = if !ties.is_null() {
        core::slice::from_raw_parts(ties, tie_len)
    } else {
        &[]
    };
    let name_slice = if !name.is_null() {
        Some(core::slice::from_raw_parts(name.cast(), namelen))
    } else {
        None
    };
    let name = name_slice.and_then(|name_slice| str::from_utf8(name_slice).ok());
    result_id_to_bindings(OUR_RUNTIME.create_object(&ObjectCreate::from(*spec), src, ties, name))
}
check_ffi_type!(twz_rt_create_object, _, _, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_map_object(id: objid, flags: map_flags) -> map_result {
    OUR_RUNTIME
        .map_object(id.into(), MapFlags::from_bits_truncate(flags))
        .into()
}
check_ffi_type!(twz_rt_map_object, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_release_handle(
    handle: *mut object_handle,
    flags: release_flags,
) {
    OUR_RUNTIME.release_handle(handle, flags)
}
check_ffi_type!(twz_rt_release_handle, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_object_cmd(
    handle: *mut object_handle,
    cmd: object_cmd,
    arg: *mut c_void,
) -> twz_error {
    match OUR_RUNTIME.object_cmd(handle, cmd, arg) {
        Ok(_) => 0,
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_object_cmd, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_update_handle(handle: *mut object_handle) -> twz_error {
    match OUR_RUNTIME.update_handle(handle) {
        Ok(_) => 0,
        Err(e) => e.raw(),
    }
}
check_ffi_type!(twz_rt_update_handle, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_object_handle(ptr: *mut c_void) -> object_handle {
    OUR_RUNTIME
        .get_object_handle_from_ptr(ptr.cast())
        .unwrap_or(object_handle::default())
}
check_ffi_type!(twz_rt_get_object_handle, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_insert_fot(
    handle: *mut object_handle,
    fote: *mut c_void,
) -> u32_result {
    OUR_RUNTIME.insert_fot(handle, fote.cast()).into()
}
check_ffi_type!(twz_rt_insert_fot, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_resolve_fot(
    handle: *mut object_handle,
    idx: u64,
    valid_len: usize,
    map_flags: map_flags,
) -> map_result {
    OUR_RUNTIME
        .resolve_fot(
            handle,
            idx,
            valid_len,
            MapFlags::from_bits_truncate(map_flags),
        )
        .into()
}
check_ffi_type!(twz_rt_resolve_fot, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_resolve_fot_local(
    ptr: *mut c_void,
    idx: u64,
    valid_len: usize,
    map_flags: map_flags,
) -> *mut c_void {
    OUR_RUNTIME
        .resolve_fot_local(
            ptr.cast(),
            idx,
            valid_len,
            MapFlags::from_bits_truncate(map_flags),
        )
        .cast()
}
check_ffi_type!(twz_rt_resolve_fot_local, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn __twz_rt_map_two_objects(
    id_1: objid,
    flags_1: map_flags,
    id_2: objid,
    flags_2: map_flags,
    res_1: *mut map_result,
    res_2: *mut map_result,
) {
    unsafe {
        match OUR_RUNTIME.map_two_objects(
            id_1.into(),
            MapFlags::from_bits_truncate(flags_1),
            id_2.into(),
            MapFlags::from_bits_truncate(flags_2),
        ) {
            Ok((r1, r2)) => {
                res_1.write(Ok(r1).into());
                res_2.write(Ok(r2).into());
            }
            Err(e) => {
                res_1.write(Err(e).into());
                res_2.write(Err(e).into());
            }
        }
    }
}
check_ffi_type!(__twz_rt_map_two_objects, _, _, _, _, _, _);

// time.h

use twizzler_rt_abi::bindings::duration;
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_monotonic_time() -> duration {
    OUR_RUNTIME.get_monotonic().into()
}
check_ffi_type!(twz_rt_get_monotonic_time);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_system_time() -> duration {
    OUR_RUNTIME.get_system_time().into()
}
check_ffi_type!(twz_rt_get_system_time);

// debug.h

use twizzler_rt_abi::bindings::{dl_phdr_info, loaded_image, loaded_image_id};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_loaded_image(
    id: loaded_image_id,
    li: *mut loaded_image,
) -> bool {
    let Some(image_info) = OUR_RUNTIME.get_image_info(id) else {
        return false;
    };
    unsafe {
        li.write(image_info);
    }
    true
}
check_ffi_type!(twz_rt_get_loaded_image, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_iter_phdr(
    cb: ::core::option::Option<
        unsafe extern "C-unwind" fn(
            arg1: *const dl_phdr_info,
            size: usize,
            data: *mut ::core::ffi::c_void,
        ) -> ::core::ffi::c_int,
    >,
    data: *mut ::core::ffi::c_void,
) -> ::core::ffi::c_int {
    OUR_RUNTIME
        .iterate_phdr(&mut |info| cb.unwrap()(&info, core::mem::size_of::<dl_phdr_info>(), data))
}
check_ffi_type!(twz_rt_iter_phdr, _, _);

// info.h
use twizzler_rt_abi::bindings::system_info;
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_sysinfo() -> system_info {
    OUR_RUNTIME.sysinfo()
}
check_ffi_type!(twz_rt_get_sysinfo);

// exec.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_exec_spawn(
    args: *const twizzler_rt_abi::bindings::exec_spawn_args,
) -> open_result {
    OUR_RUNTIME.exec_spawn(args.as_ref().unwrap()).into()
}
check_ffi_type!(twz_rt_exec_spawn, _);

// random.h

use twizzler_rt_abi::bindings::get_random_flags;
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_random(
    buf: *mut ::core::ffi::c_char,
    len: usize,
    flags: get_random_flags,
) -> usize {
    OUR_RUNTIME.get_random(
        unsafe { core::slice::from_raw_parts_mut(buf.cast(), len) },
        flags.into(),
    )
}
check_ffi_type!(twz_rt_get_random, _, _, _);

// additional definitions for C
#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn malloc(len: usize) -> *mut core::ffi::c_void {
    warn!("called c:malloc with len = {}: not yet implemented", len);
    core::ptr::null_mut()
}

#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn free(ptr: *mut core::ffi::c_void) {
    warn!("called c:free with ptr = {:p}: not yet implemented", ptr);
}

#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char {
    let n = unsafe { CStr::from_ptr(name.cast()) };
    OUR_RUNTIME.cgetenv(n)
}

#[no_mangle]
pub unsafe extern "C-unwind" fn dl_iterate_phdr(
    cb: ::core::option::Option<
        unsafe extern "C-unwind" fn(
            arg1: *const dl_phdr_info,
            size: usize,
            data: *mut ::core::ffi::c_void,
        ) -> ::core::ffi::c_int,
    >,
    data: *mut core::ffi::c_void,
) -> i32 {
    twz_rt_iter_phdr(cb, data)
}

#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn fwrite(
    ptr: *const core::ffi::c_void,
    len: usize,
    nitems: usize,
    file: *const core::ffi::c_void,
) -> usize {
    twz_rt_fd_pwrite(1, ptr, len * nitems, core::ptr::null_mut());
    len * nitems
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn fprintf(
    file: *const core::ffi::c_void,
    fmt: *const core::ffi::c_char,
    args: ...
) -> i32 {
    use printf_compat::{format, output};
    let mut s = String::new();
    let bytes_written = format(fmt.cast(), args, output::fmt_write(&mut s));
    twz_rt_fd_pwrite(
        1,
        s.as_bytes().as_ptr().cast(),
        s.as_bytes().len(),
        core::ptr::null_mut(),
    );
    bytes_written
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __monitor_get_slot() -> isize {
    match OUR_RUNTIME.allocate_slot() {
        Some(s) => s.try_into().unwrap_or(-1),
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __monitor_release_slot(slot: usize) {
    OUR_RUNTIME.release_slot(slot);
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __monitor_release_pair(one: usize, two: usize) {
    OUR_RUNTIME.release_pair((one, two));
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __monitor_get_slot_pair(one: *mut usize, two: *mut usize) -> bool {
    let Some((a, b)) = OUR_RUNTIME.allocate_pair() else {
        return false;
    };
    one.write(a);
    two.write(b);
    true
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __monitor_ready() {
    OUR_RUNTIME.set_runtime_ready();
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __is_monitor_ready() -> bool {
    OUR_RUNTIME.state().contains(crate::RuntimeState::READY)
}

#[no_mangle]
pub unsafe extern "C-unwind" fn __is_monitor() -> *mut c_void {
    OUR_RUNTIME.is_monitor().unwrap_or(core::ptr::null_mut())
}

#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn _ZdlPv() {}

#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn _ZdlPvj() {}

#[linkage = "weak"]
#[no_mangle]
pub unsafe extern "C-unwind" fn _ZdlPvm() {}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_error() -> *const c_char {
    take_dl_error()
}

use core::ffi::c_int;
use std::cell::RefCell;

#[thread_local]
static DLAPI_ERROR: RefCell<Option<std::ffi::CString>> = const { RefCell::new(None) };

fn set_dl_error(msg: impl Into<Vec<u8>>) {
    *DLAPI_ERROR.borrow_mut() = std::ffi::CString::new(msg).ok();
}

fn take_dl_error() -> *const c_char {
    DLAPI_ERROR
        .borrow_mut()
        .take()
        .map(|s| s.into_raw() as *const c_char)
        .unwrap_or(core::ptr::null())
}

/// The __dlapi_symbol struct as defined by mlibc's dlfcn.cpp.
#[repr(C)]
struct DlapiSymbol {
    file: *const c_char,
    base: *mut c_void,
    symbol: *const c_char,
    address: *mut c_void,
    elf_symbol: *const c_void,
    link_map: *mut c_void,
}

/// Encode a `Descriptor` as a non-null `void*` handle (offset by 1 so descriptor 0 != NULL).
fn desc_to_handle(desc: secgate::util::Descriptor) -> *mut c_void {
    (desc as usize + 1) as *mut c_void
}

/// Decode a handle back to a descriptor. Returns `None` for NULL (RTLD_DEFAULT).
fn handle_to_desc(handle: *const c_void) -> Option<secgate::util::Descriptor> {
    let v = handle as usize;
    if v == 0 {
        None
    } else {
        Some((v - 1) as secgate::util::Descriptor)
    }
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_open(
    filename: *const c_char,
    _flags: c_int,
    _return_addr: *const c_void,
) -> *mut c_void {
    //twizzler_abi::klog_println!("called __dlapi_open with filename = {:p}", filename);
    if filename.is_null() {
        return core::ptr::null_mut(); // RTLD_DEFAULT sentinel
    }
    let name = match unsafe { std::ffi::CStr::from_ptr(filename) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_dl_error("dlopen: invalid UTF-8 in filename");
            return core::ptr::null_mut();
        }
    };
    let id = OUR_RUNTIME
        .resolve_name(twizzler_rt_abi::fd::NameResolver::Default, name.as_bytes())
        .ok();
    match monitor_api::LibraryLoader::new(name, id).load() {
        Ok(desc) => desc_to_handle(desc.into_raw()),
        Err(e) => {
            set_dl_error(format!("dlopen: library '{}' not found: {:?}", name, e));
            core::ptr::null_mut()
        }
    }
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_resolve(
    handle: *const c_void,
    symbol: *const c_char,
    _return_addr: *const c_void,
    _version: *const c_char,
) -> *mut c_void {
    let sym_name = match unsafe { std::ffi::CStr::from_ptr(symbol) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_dl_error("dlsym: invalid UTF-8 in symbol name");
            return core::ptr::null_mut();
        }
    };
    let lib_desc = handle_to_desc(handle);
    match monitor_api::lookup_symbol_by_name(lib_desc, sym_name) {
        Ok(addr) if addr != 0 => addr as *mut c_void,
        Ok(_) => {
            set_dl_error(format!("dlsym: symbol '{}' resolved to NULL", sym_name));
            core::ptr::null_mut()
        }
        Err(e) => {
            set_dl_error(format!("dlsym: symbol '{}' not found: {:?}", sym_name, e));
            core::ptr::null_mut()
        }
    }
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_reverse(
    ptr: *const c_void,
    out: *mut c_void, // actually *mut DlapiSymbol
) -> c_int {
    //twizzler_abi::klog_println!("called __dlapi_reverse with ptr = {:p}", ptr);
    if out.is_null() {
        return 1;
    }
    let out = unsafe { &mut *(out as *mut DlapiSymbol) };

    // Iterate libraries in the current compartment until we find one whose mapped
    // range contains `ptr`.
    let mut lib_n: usize = 0;
    loop {
        let desc = match monitor_api::monitor_rt_get_library_handle(None, lib_n) {
            Ok(d) => d,
            Err(_) => break,
        };
        let raw = match monitor_api::monitor_rt_get_library_info(desc) {
            Ok(r) => r,
            Err(_) => {
                let _ = monitor_api::monitor_rt_drop_library_handle(desc);
                break;
            }
        };
        let lib_info = monitor_api::LibraryInfo::from_raw(raw);
        let base = lib_info.dl_info.addr as *const u8;
        let len = lib_info.len;

        if !base.is_null()
            && (ptr as usize) >= (base as usize)
            && (ptr as usize) < (base as usize + MAX_SIZE * 2)
        {
            // TODO: this leaks.
            let name_cstring = std::ffi::CString::new(lib_info.name.clone()).unwrap_or_default();
            let name_ptr: *const c_char = name_cstring.into_raw();
            let _ = monitor_api::monitor_rt_drop_library_handle(desc);
            out.file = name_ptr;
            out.base = base as *mut c_void;
            out.symbol = core::ptr::null();
            out.address = core::ptr::null_mut();
            out.elf_symbol = core::ptr::null();
            out.link_map = lib_info.link_map.0.ld.cast();
            return 0;
        }

        let _ = monitor_api::monitor_rt_drop_library_handle(desc);
        lib_n += 1;
    }
    1 // not found
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_close(handle: *const c_void) -> c_int {
    if handle.is_null() {
        return 0;
    }
    let Some(desc) = handle_to_desc(handle) else {
        return 0;
    };
    match monitor_api::monitor_rt_drop_library_handle(desc) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_find_object() -> *const c_char {
    twizzler_abi::klog_println!("called __dlapi_find_object: not yet implemented");
    core::ptr::null()
}

#[linkage = "weak"]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __dlapi_get_tls() -> *const c_char {
    twizzler_abi::klog_println!("called __dlapi_get_tls: not yet implemented");
    core::ptr::null()
}
