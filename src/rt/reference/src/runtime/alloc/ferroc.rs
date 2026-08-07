use std::{
    alloc::{AllocError, GlobalAlloc},
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::talc::{LocalAllocator, LOCAL_ALLOCATOR};

/// DIAG (multiputbug): registry of the chunks we have handed ferroc, so a `dealloc` can check
/// that a pointer bound for ferroc actually lives in memory ferroc owns. Fixed-size and
/// atomic-only: this is consulted from inside the allocator and must never allocate.
const MAX_CHUNKS: usize = 64;
static CHUNK_BASE: [AtomicUsize; MAX_CHUNKS] = [const { AtomicUsize::new(0) }; MAX_CHUNKS];
static CHUNK_LEN: [AtomicUsize; MAX_CHUNKS] = [const { AtomicUsize::new(0) }; MAX_CHUNKS];
static CHUNK_COUNT: AtomicUsize = AtomicUsize::new(0);

fn record_chunk(base: usize, len: usize) {
    let idx = CHUNK_COUNT.fetch_add(1, Ordering::SeqCst);
    if idx < MAX_CHUNKS {
        CHUNK_BASE[idx].store(base, Ordering::SeqCst);
        CHUNK_LEN[idx].store(len, Ordering::SeqCst);
    }
}

/// DIAG (multiputbug): the first shard header sits at chunk+0x80 and its free-list head at
/// chunk+0xa0. Catch the moment that head stops being a canonical pointer, so the corrupting
/// write is bracketed between two allocations instead of found at the next pop.
static CORRUPT_REPORTED: AtomicBool = AtomicBool::new(false);
static IN_CHECK: AtomicBool = AtomicBool::new(false);
const NONCANONICAL: usize = 0x0000_8000_0000_0000;

const RING: usize = 128;
static RING_PTR: [AtomicUsize; RING] = [const { AtomicUsize::new(0) }; RING];
static RING_LEN: [AtomicUsize; RING] = [const { AtomicUsize::new(0) }; RING];
static RING_KIND: [AtomicUsize; RING] = [const { AtomicUsize::new(0) }; RING];
static RING_TCB: [AtomicUsize; RING] = [const { AtomicUsize::new(0) }; RING];
static RING_IDX: AtomicUsize = AtomicUsize::new(0);

/// ferroc is configured with per-thread heaps, so which thread an allocation came from decides
/// whether two overlapping allocations imply shared heap state.
fn tcb() -> usize {
    unsafe { dynlink::tls::get_current_thread_control_block::<()>() as usize }
}

fn record(ptr: usize, len: usize, kind: usize) {
    let i = RING_IDX.fetch_add(1, Ordering::SeqCst) % RING;
    RING_PTR[i].store(ptr, Ordering::SeqCst);
    RING_LEN[i].store(len, Ordering::SeqCst);
    RING_KIND[i].store(kind, Ordering::SeqCst);
    RING_TCB[i].store(tcb(), Ordering::SeqCst);
}

pub fn record_alloc(ptr: usize, len: usize) {
    record(ptr, len, 0);
}

/// `to_ferroc` distinguishes a free that reached ferroc from one the routing in `dealloc`
/// dropped on the floor, since those leak rather than free. Only frees of memory inside a
/// ferroc chunk are recorded; early-talc pointers are skipped in bulk and would swamp the ring.
pub fn record_free(ptr: usize, len: usize, to_ferroc: bool) {
    if ptr_in_ferroc_chunk(ptr) {
        record(ptr, len, if to_ferroc { 2 } else { 1 });
    }
}

/// Backtrace without allocating: `Backtrace::force_capture` re-enters the allocator, which is
/// fatal once the free list is already corrupt.
#[inline(never)]
fn print_frames() {
    let mut fp: usize;
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) fp) };
    for _ in 0..12 {
        if fp == 0 || fp % 8 != 0 || fp >= NONCANONICAL {
            break;
        }
        let next = unsafe { (fp as *const usize).read_volatile() };
        let ret = unsafe { (fp as *const usize).add(1).read_volatile() };
        twizzler_abi::klog_println!("  frame ret {:x}", ret);
        if next <= fp {
            break;
        }
        fp = next;
    }
}

fn dump_ring() {
    let n = RING_IDX.load(Ordering::SeqCst);
    let start = n.saturating_sub(RING);
    for i in start..n {
        let s = i % RING;
        let kind = match RING_KIND[s].load(Ordering::SeqCst) {
            0 => "alloc",
            1 => "free-skipped",
            _ => "free-ferroc",
        };
        twizzler_abi::klog_println!(
            "  recent {} {} ptr {:x} len {:x} tcb {:x}",
            i,
            kind,
            RING_PTR[s].load(Ordering::SeqCst),
            RING_LEN[s].load(Ordering::SeqCst),
            RING_TCB[s].load(Ordering::SeqCst)
        );
    }
}

pub fn check_shard_free() {
    if CORRUPT_REPORTED.load(Ordering::Relaxed) || IN_CHECK.swap(true, Ordering::SeqCst) {
        return;
    }
    let n = core::cmp::min(CHUNK_COUNT.load(Ordering::SeqCst), MAX_CHUNKS);
    for i in 0..n {
        let base = CHUNK_BASE[i].load(Ordering::SeqCst);
        if base == 0 || CHUNK_LEN[i].load(Ordering::SeqCst) < 0x400000 {
            continue;
        }
        let free = unsafe { ((base + 0xa0) as *const usize).read_volatile() };
        if free != 0 && free >= NONCANONICAL {
            CORRUPT_REPORTED.store(true, Ordering::SeqCst);
            IN_CHECK.store(false, Ordering::SeqCst);
            twizzler_abi::klog_println!(
                "FERROC-SHARD-FREE-CORRUPT: chunk {:x} free {:x}",
                base,
                free
            );
            print_frames();
            dump_ring();
            return;
        }
    }
    IN_CHECK.store(false, Ordering::SeqCst);
}

/// DIAG (Mode L): every large live allocation, so each new one can be checked against them.
/// Mode L shows a block's tail holding another block's bytes at the identical offset, and the
/// test's own check found no overlap among *its* chunks -- but it cannot see allocations made by
/// the runtime or by other threads. This widens the check to every allocation in the process.
/// Allocation-free by construction: it runs inside the allocator.
const LIVE_MAX: usize = 2048;
const LARGE_ALLOC: usize = 64 << 10;
static LIVE_PTR: [AtomicUsize; LIVE_MAX] = [const { AtomicUsize::new(0) }; LIVE_MAX];
static LIVE_LEN: [AtomicUsize; LIVE_MAX] = [const { AtomicUsize::new(0) }; LIVE_MAX];
static OVERLAP_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn note_alloc(ptr: usize, len: usize) {
    if ptr == 0 || len < LARGE_ALLOC {
        return;
    }
    let mut slot = usize::MAX;
    for i in 0..LIVE_MAX {
        let p = LIVE_PTR[i].load(Ordering::SeqCst);
        if p == 0 {
            if slot == usize::MAX {
                slot = i;
            }
            continue;
        }
        let l = LIVE_LEN[i].load(Ordering::SeqCst);
        if ptr < p.wrapping_add(l) && p < ptr.wrapping_add(len) {
            if OVERLAP_COUNT.fetch_add(1, Ordering::SeqCst) < 16 {
                twizzler_abi::klog_println!(
                    "FERROC-OVERLAP: new {:x}+{:x} overlaps live {:x}+{:x} (tcb {:x})",
                    ptr,
                    len,
                    p,
                    l,
                    tcb()
                );
            }
        }
    }
    if slot != usize::MAX {
        LIVE_PTR[slot].store(ptr, Ordering::SeqCst);
        LIVE_LEN[slot].store(len, Ordering::SeqCst);
    }
}

pub fn note_free(ptr: usize, len: usize) {
    if ptr == 0 || len < LARGE_ALLOC {
        return;
    }
    for i in 0..LIVE_MAX {
        if LIVE_PTR[i].load(Ordering::SeqCst) == ptr {
            LIVE_PTR[i].store(0, Ordering::SeqCst);
            LIVE_LEN[i].store(0, Ordering::SeqCst);
            return;
        }
    }
}

/// True if `ptr` falls inside a chunk we gave ferroc. Returns true when the registry has
/// overflowed, so an over-long run degrades to "no opinion" rather than false alarms.
pub fn ptr_in_ferroc_chunk(ptr: usize) -> bool {
    let n = CHUNK_COUNT.load(Ordering::SeqCst);
    if n > MAX_CHUNKS {
        return true;
    }
    for i in 0..n {
        let base = CHUNK_BASE[i].load(Ordering::SeqCst);
        let len = CHUNK_LEN[i].load(Ordering::SeqCst);
        if base != 0 && ptr >= base && ptr < base + len {
            return true;
        }
    }
    false
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
    const IS_ZEROED: bool = false;

    type Handle = &'static LocalAllocator;

    type Error = AllocError;

    fn allocate(
        &self,
        layout: std::alloc::Layout,
        _commit: bool,
    ) -> Result<ferroc::base::Chunk<Self>, Self::Error> {
        let ptr = unsafe { self.local_alloc.alloc(layout) };
        // DIAG (multiputbug): ferroc finds a block's owning slab by masking to SLAB_SIZE
        // (slab.rs:134), and only checks that we honored the requested alignment under
        // debug_assert! (arena.rs:123), which is compiled out in release. Verify it on our side
        // of the boundary. Logging rather than asserting: a panic here would re-enter the
        // allocator to format its message.
        if !ptr.is_null() && (ptr as usize) % layout.align() != 0 {
            twizzler_abi::klog_println!(
                "FERROC-BASE-MISALIGN: ptr {:p} size {:x} align {:x}",
                ptr,
                layout.size(),
                layout.align()
            );
        } else {
            twizzler_abi::klog_println!(
                "ferroc-base-chunk: ptr {:p} size {:x} align {:x}",
                ptr,
                layout.size(),
                layout.align()
            );
            record_chunk(ptr as usize, layout.size());
        }
        Ok(unsafe {
            Chunk::new(
                NonNull::new(ptr).ok_or(AllocError)?,
                layout,
                self.local_alloc,
            )
        })
    }

    unsafe fn deallocate(chunk: &mut ferroc::base::Chunk<Self>) {
        twizzler_abi::klog_println!("dealloc: {:p} {:x}", chunk.pointer(), chunk.layout().size());
        chunk
            .handle
            .dealloc(chunk.pointer().cast::<u8>().as_ptr(), chunk.layout());
    }

    unsafe fn commit(&self, ptr: NonNull<[u8]>) -> Result<(), Self::Error> {
        let _ = ptr;
        Ok(())
    }

    unsafe fn decommit(&self, ptr: NonNull<[u8]>) {
        twizzler_abi::klog_println!("decommit: {:p} {:x}", ptr, ptr.len());
        let _ = ptr;
    }
}

ferroc::config!(pub TwzFerroc => TwzFerrocBase: pthread);
