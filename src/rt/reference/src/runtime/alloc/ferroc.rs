use std::{
    alloc::{AllocError, GlobalAlloc},
    ptr::NonNull,
};

use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{sys_object_copy, ObjectSource},
};

use super::talc::{LocalAllocator, LOCAL_ALLOCATOR};

/// Hand the frames under `[ptr, ptr + len)` back to the kernel.
///
/// `sys_object_copy`'s zeroing source unmaps every whole page the range covers, so those frames are
/// freed and the next touch faults a fresh zero page. That is the object-system equivalent of the
/// `MADV_DONTNEED` ferroc's own mmap base uses for `decommit`, and it is the only way a page that
/// faulted into a long-lived heap object is ever given back -- without it the heap only ever grows
/// to its high-water mark.
///
/// Best-effort, like `madvise`: a range whose object cannot be named, or one the kernel rejects,
/// simply keeps its memory. Nothing depends on the range reading zero afterwards -- see
/// `IS_ZEROED`.
/// DIAG: `[decommit hook, deallocate hook, ranges entered, id lookup failed, bytes declined]`.
///
/// `sys_object_copy` reads 0 calls for a whole boot even across `l2d`, which allocates, touches and
/// frees 2 MiB forty times. Reading the code, that should retire a huge slab per iteration and
/// reach one of ferroc's two base hooks each time. A zero can mean the hooks are never called or
/// that they are called and this function declines -- `get_id_from_ptr` returning `None` is a
/// silent early return by design -- and those have fixes in different files. Counting both ends
/// separates them.
///
/// Entries 5-7 are the *other* end of the same question: how much memory ferroc has taken from
/// talc as base chunks, and how much it has given back. `hook_dealloc` reading zero says chunks are
/// never returned; `base_alloc_bytes` says how much that is worth. Growth in a `note=heap` object
/// with these flat is talc reusing an address range whose pages were already faulted in -- a
/// different mechanism from ferroc asking for more.
pub(crate) static DECOMMIT_STATS: [core::sync::atomic::AtomicU64; 8] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 8];

pub(crate) const S_BASE_ALLOC_CNT: usize = 5;
pub(crate) const S_BASE_ALLOC_BYTES: usize = 6;
pub(crate) const S_BASE_DEALLOC_BYTES: usize = 7;

fn bump(i: usize, by: u64) {
    DECOMMIT_STATS[i].fetch_add(by, core::sync::atomic::Ordering::Relaxed);
}

/// Diagnostic readout for [`DECOMMIT_STATS`]; `out` must have room for 8. Not part of the runtime
/// ABI.
#[no_mangle]
pub extern "C-unwind" fn __twz_rt_diag_decommit_stats(out: *mut u64) {
    for (i, c) in DECOMMIT_STATS.iter().enumerate() {
        unsafe { *out.add(i) = c.load(core::sync::atomic::Ordering::Relaxed) };
    }
}

unsafe fn decommit_range(ptr: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    bump(2, 1);
    if let Some(id) = LOCAL_ALLOCATOR.get_id_from_ptr(ptr) {
        let zero = ObjectSource::new_zero((ptr as usize % MAX_SIZE) as u64, len);
        let _ = sys_object_copy(id, &[zero.into()]);
    } else {
        bump(3, 1);
        bump(4, len as u64);
    }
}

pub struct TwzFerrocBase {
    pub local_alloc: &'static LocalAllocator,
}

impl TwzFerrocBase {
    pub const fn new() -> Self {
        Self {
            local_alloc: &LOCAL_ALLOCATOR,
        }
    }
}

unsafe impl ferroc::base::BaseAlloc for TwzFerrocBase {
    // True: `decommit_range` zeroes every range it is given, so a re-committed range reads zero and
    // `calloc` can skip its memset.
    //
    // This was `false` until 2026-08-18 because of a kernel bug, not anything in ferroc:
    // `Table::setup_zero_range` advanced its walk cursor twice per level-1 region, so any decommit
    // crossing a 2 MiB boundary zeroed only the first region and still returned `Ok(())`. Slabs are
    // 4 MiB *and* slab-aligned, so every slab decommit crossed one and roughly half of what the
    // runtime believed it was returning stayed dirty. Validated with this constant `true`: 6/6 runs
    // and zero `post_alloc` violations, against 0/2 on `debug-kvm-smp1` before (tag `zerofix3`),
    // plus a control that re-introduced only the double-advance and failed at exactly the 2 MiB
    // boundary. See `ferroc.md`.
    //
    // **Any change here needs a debug arm.** ferroc's zero check is a `debug_assert!`
    // (`heap.rs:376`), so a release build cannot tell you this is wrong -- it will quietly hand
    // stale bytes to `calloc` with every test green.
    const IS_ZEROED: bool = true;

    type Handle = &'static LocalAllocator;

    type Error = AllocError;

    fn allocate(
        &self,
        layout: std::alloc::Layout,
        _commit: bool,
    ) -> Result<ferroc::base::Chunk<Self>, Self::Error> {
        bump(S_BASE_ALLOC_CNT, 1);
        bump(S_BASE_ALLOC_BYTES, layout.size() as u64);
        let ptr = unsafe { self.local_alloc.alloc(layout) };
        // ferroc finds a block's owning slab by masking to SLAB_SIZE (slab.rs:134), and only
        // checks that we honored the requested alignment under `debug_assert!` (arena.rs:123),
        // which is compiled out in release. Verify it on our side of the boundary. Logging rather
        // than asserting: a panic here would re-enter the allocator to format its message.
        if !ptr.is_null() && (ptr as usize) % layout.align() != 0 {
            twizzler_abi::klog_println!(
                "FERROC-BASE-MISALIGN: ptr {:p} size {:x} align {:x}",
                ptr,
                layout.size(),
                layout.align()
            );
        }
        Ok(unsafe {
            ferroc::base::Chunk::new(
                NonNull::new(ptr).ok_or(AllocError)?,
                layout,
                self.local_alloc,
            )
        })
    }

    unsafe fn deallocate(chunk: &mut ferroc::base::Chunk<Self>) {
        bump(1, 1);
        let ptr = chunk.pointer().cast::<u8>().as_ptr();
        let layout = chunk.layout();
        bump(S_BASE_DEALLOC_BYTES, layout.size() as u64);
        // The last point at which these frames can be returned. A chunk whose slab came from
        // `SlabSource::Base` is freed by dropping it back to talc and never passes through
        // `decommit` at all, and talc reuses the address range without ever freeing the pages
        // under it -- measured as `l2d`'s 512 pages/iter with `trk.freed` and `tlb_shootdowns`
        // both flat at zero, i.e. the syscall was never reached. ferroc's mmap base gets this for
        // free, since its `deallocate` munmaps. Ordered before `dealloc` so that talc writes its
        // free-list metadata into the range afterwards, faulting it back in.
        unsafe { decommit_range(ptr, layout.size()) };
        chunk.handle.dealloc(ptr, layout);
    }

    unsafe fn commit(&self, ptr: NonNull<[u8]>) -> Result<(), Self::Error> {
        // Nothing to do: a decommitted range faults back in on its own, zeroed.
        let _ = ptr;
        Ok(())
    }

    unsafe fn decommit(&self, ptr: NonNull<[u8]>) {
        bump(0, 1);
        let len = ptr.len();
        unsafe { decommit_range(ptr.cast::<u8>().as_ptr(), len) };
    }
}

ferroc::config!(pub TwzFerroc => TwzFerrocBase: pthread);
