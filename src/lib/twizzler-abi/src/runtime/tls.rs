#[allow(dead_code)]
const MIN_TLS_ALIGN: usize = 16;
use core::alloc::Layout;

use super::__twz_get_runtime;
pub(crate) fn init_tls() -> Option<u64> {
    new_thread_tls().map(|(s, _, _, _)| s as u64)
}

//let (tls_set, tls_base, tls_len, tls_align) = crate::rt1::new_thread_tls();
pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    crate::arch::new_thread_tls()
}

#[allow(dead_code)]
pub(crate) fn tls_variant1() -> Option<(usize, *mut u8, usize, usize)> {
    unsafe {
        TLS_INFO.as_ref().map(|tls_template| {
            // TODO: reserved region may be arch specific. aarch64 reserves two
            // words after the thread pointer (TP), before any TLS blocks
            let reserved_bytes = core::mem::size_of::<*const u64>() * 2;
            // the size of the TLS region in memory
            let tls_size = tls_template.memsz + reserved_bytes;

            // generate a layout where the size is rounded up if not aligned
            let layout = crate::internal_unwrap(
                Layout::from_size_align(tls_size, tls_template.align).ok(),
                "failed to unwrap TLS layout",
            );

            // allocate a region of memory for the thread-local data initialized to zero
            let runtime = __twz_get_runtime();
            let tcb_base = runtime.default_allocator().alloc_zeroed(layout);
            if tcb_base.is_null() {
                crate::print_err("failed to allocate TLS");
                runtime.abort();
            }

            // Architechtures that use TLS Variant I (e.g. ARM) have the thread pointer
            // point to the start of the TCB and thread-local vars are defined
            // before this in higher memory addresses. So accessing a thread
            // local var adds some offset to the thread pointer
            //
            // we need a pointer offset of reserved_bytes. add here increments
            // the pointer offset by sizeof u8 bytes.
            let tls_base = tcb_base.add(reserved_bytes);
            // copy from the ELF TLS segment to the allocated region of memory
            core::ptr::copy_nonoverlapping(
                tls_template.template_start,
                tls_base,
                tls_template.filsz,
            );

            // the TP points to the base of the TCB which exists in lower memory.
            (tcb_base as usize, tcb_base, layout.size(), layout.align())
        })
    }
}

#[allow(dead_code)]
pub(crate) fn tls_variant2() -> Option<(usize, *mut u8, usize, usize)> {
    unsafe {
        TLS_INFO.as_ref().map(|info| {
            let mut tls_size = info.memsz;
            tls_size += (((!tls_size) + 1) - (info.template_start as usize)) & (info.align - 1);
            let offset = tls_size;
            let tls_align = core::cmp::max(info.align, MIN_TLS_ALIGN);
            let full_tls_size =
                core::mem::size_of::<*const u8>() + tls_size + tls_align + MIN_TLS_ALIGN - 1
                    & ((!MIN_TLS_ALIGN) + 1);

            let layout = crate::internal_unwrap(
                Layout::from_size_align(full_tls_size, tls_align).ok(),
                "failed to unwrap TLS layout",
            );
            let runtime = __twz_get_runtime();
            let tls = runtime.default_allocator().alloc_zeroed(layout);
            if tls.is_null() {
                crate::print_err("failed to allocate TLS");
                runtime.abort();
            }
            let mem = tls.add(tls_size).sub((tls as usize) & (tls_align - 1));
            core::ptr::copy_nonoverlapping(info.template_start, mem.sub(offset), info.filsz);
            *(mem as *mut u64) = mem as u64;
            (mem as usize, tls, layout.size(), layout.align())
        })
    }
}

pub(crate) struct TlsInfo {
    pub template_start: *const u8,
    pub memsz: usize,
    pub filsz: usize,
    pub align: usize,
}

static mut TLS_INFO: Option<TlsInfo> = None;

pub(super) fn set_tls_info(info: TlsInfo) {
    unsafe {
        TLS_INFO = Some(info);
    }
}
