//! Implements TLS-related start up and new thread functionality.

#![allow(static_mut_refs)]

#[allow(dead_code)]
const MIN_TLS_ALIGN: usize = 16;
use core::alloc::Layout;

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
            // The block follows the 16 reserved bytes at the thread pointer, padded so that it
            // is congruent to the template address modulo its alignment: that is where lld's
            // local-exec offsets point, assuming TP itself is aligned.
            let align = tls_template.align.max(1);
            let block_offset =
                16 + ((tls_template.template_start as usize).wrapping_sub(16) & (align - 1));
            let layout = crate::internal_unwrap(
                Layout::from_size_align(block_offset + tls_template.memsz, align.max(16)).ok(),
                "failed to unwrap TLS layout",
            );

            let tcb_base = super::OUR_RUNTIME.default_allocator().alloc_zeroed(layout);
            if tcb_base.is_null() {
                crate::print_err("failed to allocate TLS");
                super::OUR_RUNTIME.abort();
            }
            core::ptr::copy_nonoverlapping(
                tls_template.template_start,
                tcb_base.add(block_offset),
                tls_template.filsz,
            );

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
            let tls = super::OUR_RUNTIME.default_allocator().alloc_zeroed(layout);
            if tls.is_null() {
                crate::print_err("failed to allocate TLS");
                super::OUR_RUNTIME.abort();
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

unsafe impl Send for TlsInfo {}
unsafe impl Sync for TlsInfo {}

static mut TLS_INFO: Option<TlsInfo> = None;

pub(super) fn set_tls_info(info: TlsInfo) {
    unsafe {
        TLS_INFO = Some(info);
    }
}
