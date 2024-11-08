
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

// core.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_abort() {
    todo!()
}
check_ffi_type!(twz_rt_abort);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_exit(code: i32) {
    todo!()
}
check_ffi_type!(twz_rt_exit, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_pre_main_hook() -> twizzler_rt_abi::bindings::option_exit_code {
    todo!()
}
check_ffi_type!(twz_rt_pre_main_hook);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_post_main_hook() {
    todo!()
}
check_ffi_type!(twz_rt_post_main_hook);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_runtime_entry(arg: *const twizzler_rt_abi::bindings::runtime_info, std_entry: core::option::Option<unsafe extern "C-unwind" fn(arg1: twizzler_rt_abi::bindings::basic_aux) -> twizzler_rt_abi::bindings::basic_return>) {
    todo!()
}
check_ffi_type!(twz_rt_runtime_entry, _, _);

// alloc.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_malloc(sz: usize, align: usize, flags: twizzler_rt_abi::bindings::alloc_flags) -> *mut ::core::ffi::c_void {
    todo!()
}
check_ffi_type!(twz_rt_malloc, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_dealloc(
    ptr: *mut ::core::ffi::c_void,
    sz: usize,
    align: usize,
    flags: twizzler_rt_abi::bindings::alloc_flags,
) {
    todo!()
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
    todo!()
}
check_ffi_type!(twz_rt_realloc, _, _, _, _, _);

// thread.h

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wait(ptr: *mut u32, expected: twizzler_rt_abi::bindings::futex_word, timeout: twizzler_rt_abi::bindings::option_duration) -> bool {
    todo!()
}
check_ffi_type!(twz_rt_futex_wait, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_futex_wake(ptr: *mut u32, max: i64) -> bool {
    todo!()
}
check_ffi_type!(twz_rt_futex_wake, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_yield_now() {
    todo!()
}
check_ffi_type!(twz_rt_yield_now);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_set_name(name: *const ::core::ffi::c_char) {
    todo!()
}
check_ffi_type!(twz_rt_set_name, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_sleep(dur: twizzler_rt_abi::bindings::duration) {
    todo!()
}
check_ffi_type!(twz_rt_sleep, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_tls_get_addr(index: *mut twizzler_rt_abi::bindings::tls_index) -> *mut ::core::ffi::c_void {
    todo!()
}
check_ffi_type!(twz_rt_tls_get_addr, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_spawn_thread(args: twizzler_rt_abi::bindings::spawn_args) -> twizzler_rt_abi::bindings::spawn_result {
    todo!()
}
check_ffi_type!(twz_rt_spawn_thread, _);
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_join_thread(id: twizzler_rt_abi::bindings::thread_id, timeout: twizzler_rt_abi::bindings::option_duration) -> twizzler_rt_abi::bindings::join_result {
    todo!()
}
check_ffi_type!(twz_rt_join_thread, _, _);

// fd.h

use twizzler_rt_abi::bindings::{open_info, open_result, descriptor};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_open(info: open_info) -> open_result {
    todo!()
}
check_ffi_type!(twz_rt_fd_open, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_close(fd: descriptor) {
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
    todo!()
}
check_ffi_type!(twz_rt_fd_write, _, _, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_fd_seek(fd: descriptor, whence: whence, offset: i64) -> io_result {
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
    todo!()
}
check_ffi_type!(twz_rt_fd_pwritev, _, _, _, _, _);

// object.h

use twizzler_rt_abi::bindings::{rt_objid, map_flags, object_handle, map_result};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_map_object(id: rt_objid, flags: map_flags) -> map_result {
    todo!()
}
check_ffi_type!(twz_rt_map_object, _, _);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_release_handle(handle: *mut object_handle) {
    todo!()
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
    todo!()
}
check_ffi_type!(__twz_rt_map_two_objects, _, _, _, _, _, _);

// time.h

use twizzler_rt_abi::bindings::duration;
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_monotonic_time() -> duration {
    todo!()
}
check_ffi_type!(twz_rt_get_monotonic_time);

#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_system_time() -> duration {
    todo!()
}
check_ffi_type!(twz_rt_get_system_time);

// debug.h

use twizzler_rt_abi::bindings::{loaded_image, loaded_image_id, dl_phdr_info};
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_loaded_image(id: loaded_image_id, li: *mut loaded_image) -> bool {
    todo!()
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
    todo!()
}
check_ffi_type!(twz_rt_iter_phdr, _, _);


// info.h
use twizzler_rt_abi::bindings::system_info;
#[no_mangle]
pub unsafe extern "C-unwind" fn twz_rt_get_sysinfo() -> system_info {
    todo!()
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
    todo!()
}
check_ffi_type!(twz_rt_get_random, _, _, _);

// additional definitions for C

#[no_mangle]
pub unsafe extern "C-unwind" fn malloc(len: usize) -> *mut core::ffi::c_void {
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn free(ptr: *mut core::ffi::c_void) {
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char {
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn dl_iterate_phdr(cb: (), data: *mut core::ffi::c_void) -> i32 {
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn fwrite(ptr: *const core::ffi::c_void, len: usize, nitems: usize, file: core::ffi::c_void) -> usize {
    todo!()
}

#[no_mangle]
pub unsafe extern "C-unwind" fn fprintf(file: *const core::ffi::c_void, fmt: *const core::ffi::c_char, ...) -> i32 {
    todo!()
}
