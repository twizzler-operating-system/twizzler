#![allow(unused_variables)]
#![allow(non_snake_case)]

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
}

use std::ffi::{c_void, CStr};

use tracing::warn;
// core.h
use twizzler_rt_abi::bindings::option_exit_code;

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
) {
    OUR_RUNTIME.runtime_entry(arg, std_entry.unwrap_unchecked())
}
check_ffi_type!(twz_rt_runtime_entry, _, _);

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
        OUR_RUNTIME.default_allocator().alloc_zeroed(layout).cast()
    } else {
        OUR_RUNTIME.default_allocator().alloc(layout).cast()
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
    OUR_RUNTIME.default_allocator().dealloc(ptr.cast(), layout);
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
    OUR_RUNTIME
        .default_allocator()
        .realloc(ptr.cast(), layout, new_size)
        .cast()
}
check_ffi_type!(twz_rt_realloc, _, _, _, _, _);

// thread.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wait(
    ptr: *mut u32,
    expected: twizzler_rt_abi::bindings::futex_word,
    timeout: twizzler_rt_abi::bindings::option_duration,
) -> bool {
    if timeout.is_some != 0 {
        OUR_RUNTIME.futex_wait(&*ptr.cast(), expected, Some(timeout.dur.into()))
    } else {
        OUR_RUNTIME.futex_wait(&*ptr.cast(), expected, None)
    }
}
check_ffi_type!(twz_rt_futex_wait, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wake(ptr: *mut u32, max: i64) -> bool {
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
) -> twizzler_rt_abi::bindings::join_result {
    match if timeout.is_some != 0 {
        OUR_RUNTIME.join(id, Some(timeout.dur.into()))
    } else {
        OUR_RUNTIME.join(id, None)
    } {
        Ok(_) => twizzler_rt_abi::bindings::join_result_Join_Success,
        Err(e) => e.into(),
    }
}
check_ffi_type!(twz_rt_join_thread, _, _);

// fd.h

fn std_error_to_open_error(err: std::io::ErrorKind) -> twizzler_rt_abi::fd::OpenError {
    match err {
        // TODO
        _ => twizzler_rt_abi::fd::OpenError::Other,
    }
}

use twizzler_rt_abi::bindings::{descriptor, open_info, open_result};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_open(info: open_info) -> open_result {
    let name = unsafe { core::slice::from_raw_parts(info.name.cast(), info.len) };
    let name =
        core::str::from_utf8(name).map_err(|_| twizzler_rt_abi::fd::OpenError::InvalidArgument);
    match name {
        Ok(name) => OUR_RUNTIME
            .open(name, info.create.into(), info.flags.into())
            .map_err(|e| std_error_to_open_error(e.kind()))
            .into(),
        Err(e) => open_result {
            error: e as u32,
            fd: 0,
        },
    }
}
check_ffi_type!(twz_rt_fd_open, _);

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
) -> twizzler_rt_abi::bindings::fd_cmd_err {
    OUR_RUNTIME.fd_cmd(fd, cmd, arg.cast(), ret.cast())
}
check_ffi_type!(twz_rt_fd_cmd, _, _, _, _);

// io.h

fn std_error_to_io_error(err: std::io::ErrorKind) -> twizzler_rt_abi::io::IoError {
    match err {
        // TODO
        _ => twizzler_rt_abi::io::IoError::Other,
    }
}

use twizzler_rt_abi::bindings::{io_flags, io_result, io_vec, optional_offset, whence};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pread(
    fd: descriptor,
    opt_off: i64,
    buf: *mut ::core::ffi::c_void,
    len: usize,
    flags: io_flags,
) -> io_result {
    let off = if opt_off == twizzler_rt_abi::bindings::FD_POS {
        None
    } else {
        Some(opt_off as u64)
    };
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
    OUR_RUNTIME
        .fd_pread(
            fd,
            off,
            slice,
            twizzler_rt_abi::io::IoFlags::from_bits_truncate(flags),
        )
        .map_err(|e| std_error_to_io_error(e.kind()))
        .into()
}
check_ffi_type!(twz_rt_fd_pread, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pwrite(
    fd: descriptor,
    opt_off: i64,
    buf: *const ::core::ffi::c_void,
    len: usize,
    flags: io_flags,
) -> io_result {
    let off = if opt_off == twizzler_rt_abi::bindings::FD_POS {
        None
    } else {
        Some(opt_off as u64)
    };
    let slice = unsafe { core::slice::from_raw_parts(buf.cast::<u8>(), len) };
    OUR_RUNTIME
        .fd_pwrite(
            fd,
            off,
            slice,
            twizzler_rt_abi::io::IoFlags::from_bits_truncate(flags),
        )
        .map_err(|e| std_error_to_io_error(e.kind()))
        .into()
}
check_ffi_type!(twz_rt_fd_pwrite, _, _, _, _, _);

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
                value: 0,
                error: twizzler_rt_abi::bindings::io_error_IoError_SeekError,
            }
        }
    };
    OUR_RUNTIME
        .seek(fd, twz_sf_to_std_sf(seek))
        .map_err(|e| std_error_to_io_error(e.kind()))
        .into()
}
check_ffi_type!(twz_rt_fd_seek, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_preadv(
    fd: descriptor,
    opt_off: optional_offset,
    iovs: *const io_vec,
    nr_iovs: usize,
    flags: io_flags,
) -> io_result {
    let off = if opt_off == twizzler_rt_abi::bindings::FD_POS {
        None
    } else {
        Some(opt_off as u64)
    };
    let slice = unsafe { core::slice::from_raw_parts(iovs, nr_iovs) };
    OUR_RUNTIME
        .fd_preadv(
            fd,
            off,
            slice,
            twizzler_rt_abi::io::IoFlags::from_bits_truncate(flags),
        )
        .map_err(|e| std_error_to_io_error(e.kind()))
        .into()
}
check_ffi_type!(twz_rt_fd_preadv, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pwritev(
    fd: descriptor,
    opt_off: optional_offset,
    iovs: *const io_vec,
    nr_iovs: usize,
    flags: io_flags,
) -> io_result {
    let off = if opt_off == twizzler_rt_abi::bindings::FD_POS {
        None
    } else {
        Some(opt_off as u64)
    };
    let slice = unsafe { core::slice::from_raw_parts(iovs, nr_iovs) };
    OUR_RUNTIME
        .fd_pwritev(
            fd,
            off,
            slice,
            twizzler_rt_abi::io::IoFlags::from_bits_truncate(flags),
        )
        .map_err(|e| std_error_to_io_error(e.kind()))
        .into()
}
check_ffi_type!(twz_rt_fd_pwritev, _, _, _, _, _);

// object.h

use twizzler_rt_abi::{
    bindings::{map_flags, map_result, object_handle, rt_objid},
    object::MapFlags,
};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_map_object(id: rt_objid, flags: map_flags) -> map_result {
    OUR_RUNTIME
        .map_object(id.into(), MapFlags::from_bits_truncate(flags))
        .into()
}
check_ffi_type!(twz_rt_map_object, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_release_handle(handle: *mut object_handle) {
    OUR_RUNTIME.release_handle(handle)
}
check_ffi_type!(twz_rt_release_handle, _);

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
) -> i64 {
    match OUR_RUNTIME.insert_fot(handle, fote.cast()) {
        Some(x) => x as i64,
        None => -1,
    }
}
check_ffi_type!(twz_rt_insert_fot, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_resolve_fot(
    handle: *mut object_handle,
    idx: u64,
    valid_len: usize,
) -> map_result {
    OUR_RUNTIME.resolve_fot(handle, idx, valid_len).into()
}
check_ffi_type!(twz_rt_resolve_fot, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_resolve_fot_local(
    ptr: *mut c_void,
    idx: u64,
    valid_len: usize,
) -> *mut c_void {
    OUR_RUNTIME
        .resolve_fot_local(ptr.cast(), idx, valid_len)
        .cast()
}
check_ffi_type!(twz_rt_resolve_fot_local, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn __twz_rt_map_two_objects(
    id_1: rt_objid,
    flags_1: map_flags,
    id_2: rt_objid,
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

#[no_mangle]
pub unsafe extern "C-unwind" fn malloc(len: usize) -> *mut core::ffi::c_void {
    warn!("called c:malloc with len = {}: not yet implemented", len);
    core::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn free(ptr: *mut core::ffi::c_void) {
    warn!("called c:free with ptr = {:p}: not yet implemented", ptr);
}

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

#[no_mangle]
pub unsafe extern "C-unwind" fn fwrite(
    ptr: *const core::ffi::c_void,
    len: usize,
    nitems: usize,
    file: *const core::ffi::c_void,
) -> usize {
    twz_rt_fd_pwrite(1, twizzler_rt_abi::bindings::FD_POS, ptr, len * nitems, 0);
    len * nitems
}

#[no_mangle]
pub unsafe extern "C-unwind" fn fprintf(
    file: *const core::ffi::c_void,
    fmt: *const core::ffi::c_char,
    mut args: ...
) -> i32 {
    use printf_compat::{format, output};
    let mut s = String::new();
    let bytes_written = format(fmt.cast(), args.as_va_list(), output::fmt_write(&mut s));
    twz_rt_fd_pwrite(
        1,
        twizzler_rt_abi::bindings::FD_POS,
        s.as_bytes().as_ptr().cast(),
        s.as_bytes().len(),
        0,
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
