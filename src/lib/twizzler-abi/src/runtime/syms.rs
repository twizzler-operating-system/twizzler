#![allow(unused_variables)]


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
    }
}

use crate::runtime::OUR_RUNTIME;

// core.h

use twizzler_rt_abi::bindings::option_exit_code;

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
        Some(ec) => option_exit_code { is_some: 1, value: ec },
        None => option_exit_code { is_some: 0, value: 0 },
    }
}
check_ffi_type!(twz_rt_pre_main_hook);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_post_main_hook() {
    OUR_RUNTIME.post_main_hook()
}
check_ffi_type!(twz_rt_post_main_hook);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_runtime_entry(arg: *const twizzler_rt_abi::bindings::runtime_info, std_entry: core::option::Option<unsafe extern "C-unwind" fn(arg1: twizzler_rt_abi::bindings::basic_aux) -> twizzler_rt_abi::bindings::basic_return>) {
    OUR_RUNTIME.runtime_entry(arg, std_entry.unwrap_unchecked())
}
check_ffi_type!(twz_rt_runtime_entry, _, _);

// alloc.h

use twizzler_rt_abi::bindings::{alloc_flags, ZERO_MEMORY};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_malloc(sz: usize, align: usize, flags: alloc_flags) -> *mut ::core::ffi::c_void {
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, align) else { return core::ptr::null_mut(); };
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
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, align) else { return; };
    if flags & ZERO_MEMORY != 0 {
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u8>(), sz) };
        slice.fill(0);
        core::hint::black_box(slice);
    }
    OUR_RUNTIME.default_allocator().dealloc(ptr.cast(), layout);
}
check_ffi_type!(twz_rt_dealloc, _, _, _, _);

#[no_mangle]
pub  unsafe extern "C-unwind" fn twz_rt_realloc(
    ptr: *mut ::core::ffi::c_void,
    sz: usize,
    align: usize,
    new_size: usize,
    flags: twizzler_rt_abi::bindings::alloc_flags,
) -> *mut ::core::ffi::c_void {
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, align) else { return core::ptr::null_mut(); };
    if flags & ZERO_MEMORY != 0 {
        todo!()
    }
    OUR_RUNTIME.default_allocator().realloc(ptr.cast(), layout, new_size).cast()
}
check_ffi_type!(twz_rt_realloc, _, _, _, _, _);

// thread.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wait(ptr: *mut u32, expected: twizzler_rt_abi::bindings::futex_word, timeout: twizzler_rt_abi::bindings::option_duration) -> bool {
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
pub unsafe extern "C-unwind" fn twz_rt_tls_get_addr(index: *mut twizzler_rt_abi::bindings::tls_index) -> *mut ::core::ffi::c_void {
    OUR_RUNTIME.tls_get_addr(unsafe {&*index}).unwrap_or(core::ptr::null_mut()).cast()
}
check_ffi_type!(twz_rt_tls_get_addr, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_spawn_thread(args: twizzler_rt_abi::bindings::spawn_args) -> twizzler_rt_abi::bindings::spawn_result {
    OUR_RUNTIME.spawn(args).into()
}
check_ffi_type!(twz_rt_spawn_thread, _);
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_join_thread(id: twizzler_rt_abi::bindings::thread_id, timeout: twizzler_rt_abi::bindings::option_duration) -> twizzler_rt_abi::bindings::join_result {
    match if timeout.is_some != 0 {
        OUR_RUNTIME.join(id, Some(timeout.dur.into()))
    } else {
        OUR_RUNTIME.join(id, None)
    } {
        Ok(_) => twizzler_rt_abi::bindings::join_result_Join_Success,
        Err(e) => e.into()
    }
}
check_ffi_type!(twz_rt_join_thread, _, _);

// fd.h

use twizzler_rt_abi::bindings::{open_info, open_result, descriptor};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_open(info: open_info) -> open_result {
        crate::print_err("!! open\n");
    todo!()
}
check_ffi_type!(twz_rt_fd_open, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_close(fd: descriptor) {
        crate::print_err("!! close\n");
    todo!()
}
check_ffi_type!(twz_rt_fd_close, _);

// io.h

use twizzler_rt_abi::bindings::{io_flags, io_result, whence, optional_offset, io_vec};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_read(
    fd: descriptor,
    buf: *mut ::core::ffi::c_void,
    len: usize,
    flags: io_flags,
) -> io_result {
        crate::print_err("!! read\n");
    todo!()
}
check_ffi_type!(twz_rt_fd_read, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_write(
    fd: descriptor,
    buf: *const ::core::ffi::c_void,
    len: usize,
    flags: io_flags,
) -> io_result {
    crate::syscall::sys_kernel_console_write(unsafe {core::slice::from_raw_parts(buf.cast(), len)}, crate::syscall::KernelConsoleWriteFlags::empty());
    io_result {
        error: twizzler_rt_abi::bindings::io_error_IoError_Success,
        value: len, 
    }
}
check_ffi_type!(twz_rt_fd_write, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_seek(fd: descriptor, whence: whence, offset: i64) -> io_result {
        crate::print_err("!! seek\n");
    todo!()
}
check_ffi_type!(twz_rt_fd_seek, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_preadv(
    fd: descriptor,
    offset: optional_offset,
    iovs: *const io_vec,
    nr_iovs: usize,
    flags: io_flags,
) -> io_result {
        crate::print_err("!! pread\n");
    todo!()
}
check_ffi_type!(twz_rt_fd_preadv, _, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_pwritev(
    fd: descriptor,
    offset: optional_offset,
    iovs: *const io_vec,
    nr_iovs: usize,
    flags: io_flags,
) -> io_result {
        crate::print_err("!! pwrite\n");
    todo!()
}
check_ffi_type!(twz_rt_fd_pwritev, _, _, _, _, _);

// object.h

use twizzler_rt_abi::object::MapFlags;
use twizzler_rt_abi::bindings::{rt_objid, map_flags, object_handle, map_result};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_map_object(id: rt_objid, flags: map_flags) -> map_result {
    OUR_RUNTIME.map_object(id.into(), MapFlags::from_bits_truncate(flags)).into()
}
check_ffi_type!(twz_rt_map_object, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_release_handle(handle: *mut object_handle) {
    OUR_RUNTIME.release_handle(handle)
}
check_ffi_type!(twz_rt_release_handle, _);

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
        match OUR_RUNTIME.map_two_objects(id_1.into(), MapFlags::from_bits_truncate(flags_1), id_2.into(), MapFlags::from_bits_truncate(flags_2)) {
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

use twizzler_rt_abi::bindings::{loaded_image, loaded_image_id, dl_phdr_info};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_loaded_image(id: loaded_image_id, li: *mut loaded_image) -> bool {
    let image_info = OUR_RUNTIME.get_image_info();
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
        crate::print_err("!! phdr\n");
    todo!()
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
    OUR_RUNTIME.get_random(unsafe { core::slice::from_raw_parts_mut(buf.cast(), len) })
}
check_ffi_type!(twz_rt_get_random, _, _, _);

// additional definitions for C

#[no_mangle]
pub unsafe extern "C-unwind" fn malloc(len: usize) -> *mut core::ffi::c_void {
        crate::print_err("!! malloc\n");
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn free(ptr: *mut core::ffi::c_void) {
        crate::print_err("!! free\n");
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char {
        crate::print_err("!! getenv\n");
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn dl_iterate_phdr(cb: (), data: *mut core::ffi::c_void) -> i32 {
        crate::print_err("!! dl_iter\n");
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn fwrite(ptr: *const core::ffi::c_void, len: usize, nitems: usize, file: core::ffi::c_void) -> usize {
        crate::print_err("!! fwrite\n");
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn fprintf(file: *const core::ffi::c_void, fmt: *const core::ffi::c_char, ...) -> i32 {
        crate::print_err("!! fprintf\n");
    todo!()
}