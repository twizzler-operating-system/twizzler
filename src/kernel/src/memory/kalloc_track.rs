//! Live-block attributor for one kernel-heap size class.
//!
//! [`kalloc_census`](super::kalloc_census) names the size class that fails to balance. It cannot
//! name the 51 blocks out of 6,652 in that class that were never freed, because a per-class count
//! is a scalar. This records the *live set*: every allocation whose size falls in the armed range
//! is entered in a static table with its return-address chain, and every free of that size removes
//! it. Whatever is still in the table at a dump is the residual, with provenance.
//!
//! Three constraints shape the implementation, all learned the expensive way in this tree:
//!
//! - **It must not allocate.** It runs inside `GlobalAlloc::alloc`; allocating there re-enters the
//!   heap and, past the first heap extension, self-deadlocks on `GLOBAL_PAGE_ALLOC`. Static
//!   storage, bump-and-freelist, no `Vec`, no formatting.
//! - **It must not take a lock an allocating caller could already hold.** Its one lock is private
//!   to this module and is held only over pointer bookkeeping.
//! - **It must not print from the alloc path.** Symbolizing allocates (DWARF), and the console lock
//!   may be held by the caller. Addresses are recorded raw and dumped out of band, from the syscall
//!   that asks for them; symbolization happens on the host with addr2line.
//!
//! The early bump allocator cannot produce a false leak here. `GlobalAllocWrapper::alloc` routes
//! to `EARLY_ALLOCATOR` and returns *before* the hook, so an early block is never inserted; an
//! early block freed later reaches `record_free`, misses, and is counted in `free_miss`. The
//! structural non-freeing of that 1 MiB static region therefore inflates `free_miss` and can never
//! inflate `live`.
//!
//! **`live` is not the residual, and the difference is why every slot carries a sequence number.**
//! Let A be the allocations made inside the window, F_in the frees of those, and F_pre the frees of
//! blocks that predate the window. Then `live = A - F_in` while the census's `net_count = A - F_in
//! - F_pre`, so `live = net_count + free_miss` exactly -- an identity worth checking, and a
//!   warning.
//! A steady-state system rotates: blocks allocated in the window replace blocks allocated before
//! it, so the live set legitimately contains F_pre blocks that are simply the current generation of
//! something long-lived. What separates them is *age*. A leaked block sits at a low sequence number
//! and stays; a rotating site's live blocks are all recent. The dump therefore reports, per site,
//! the count and the oldest and newest sequence numbers in it: a site whose live blocks span the
//! whole window is retaining, one whose live blocks are all from the end of it is turning over.
//!
//! Armed and dumped entirely through `sys_kalloc_track`, with no kernel command-line flag: the
//! residual is scale-triggered, so the table only ever needs to hold the allocations made inside
//! one operation, and arming late keeps the live set small enough to be exact rather than sampled.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use twizzler_abi::syscall::{
    KALLOC_TRACK_ARM, KALLOC_TRACK_DUMP, KALLOC_TRACK_OFF, KallocTrackCtl,
};

/// Slots. One per live allocation in the armed range; overflow is counted, never silently dropped.
const NR_SLOTS: usize = 1 << 14;
/// Hash buckets. Same order as the slots, so chains stay short.
const NR_BUCKETS: usize = 1 << 14;
/// Return addresses captured per allocation. Frame 0 is inside `GlobalAlloc::alloc` itself; the
/// interesting frames start at 1, and 6 is enough to get past the `__rust_alloc`/`RawVec` shims to
/// a name that means something.
const NR_IPS: usize = 6;
/// Distinct call-site chains the dump aggregates into. Beyond this the dump reports the overflow
/// rather than merging unrelated sites.
const NR_SITES: usize = 32;
/// Live blocks whose bytes get printed. Contents identify a struct as often as a backtrace does --
/// a 128-bit `ObjID`, an `Arc` refcount pair and a kernel pointer all look like themselves.
const NR_DUMP_BYTES: usize = 24;

#[derive(Clone, Copy)]
struct Slot {
    /// 0 = free. Also the key.
    ptr: u64,
    size: u32,
    /// Index+1 of the next slot in this bucket's chain; 0 = end.
    next: u32,
    /// Insertion order within the window. Age is what separates a retained block from a rotating
    /// one, and nothing else in the table carries it.
    seq: u64,
    ips: [u64; NR_IPS],
}

const EMPTY_SLOT: Slot = Slot {
    ptr: 0,
    size: 0,
    next: 0,
    seq: 0,
    ips: [0; NR_IPS],
};

struct Table {
    /// Index+1 of the head of each chain; 0 = empty.
    buckets: [u32; NR_BUCKETS],
    slots: [Slot; NR_SLOTS],
    /// Slots never yet handed out. Bump first, then the free list, so an all-zero table is a valid
    /// initial state and the whole thing lives in .bss rather than in the kernel image.
    bump: usize,
    /// Index+1 of the head of the free list; 0 = empty.
    free: u32,
    live: u64,
    inserted: u64,
    removed: u64,
    overflow: u64,
    free_miss: u64,
}

/// Zeroed, so this is `.bss`: ~1 MiB of it, present in every kernel image but touched only when
/// armed.
static mut TABLE: Table = Table {
    buckets: [0; NR_BUCKETS],
    slots: [EMPTY_SLOT; NR_SLOTS],
    bump: 0,
    free: 0,
    live: 0,
    inserted: 0,
    removed: 0,
    overflow: 0,
    free_miss: 0,
};

/// Private to this module and held only over pointer bookkeeping -- never across an allocation, a
/// print, or any other kernel lock. Taken with interrupts off, so an interrupt that allocates on
/// this cpu cannot re-enter it.
static LOCK: AtomicBool = AtomicBool::new(false);

/// Off unless armed. An unarmed boot pays one relaxed load of a shared-read line per alloc/free,
/// which is the same price the census already charges.
static ON: AtomicBool = AtomicBool::new(false);
static LO: AtomicUsize = AtomicUsize::new(usize::MAX);
static HI: AtomicUsize = AtomicUsize::new(0);
static ARM_GEN: AtomicU64 = AtomicU64::new(0);

struct Guard;

impl Guard {
    #[inline]
    fn acquire() -> Self {
        while LOCK.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }
        Guard
    }
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        LOCK.store(false, Ordering::Release);
    }
}

#[inline]
fn hash(ptr: u64) -> usize {
    // Blocks in one size class are a fixed stride apart inside a slab, so the low bits alone
    // collide hard. Mix before masking.
    let x = ptr >> 4;
    let x = x.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    ((x >> 32) as usize) & (NR_BUCKETS - 1)
}

#[inline]
fn in_range(size: usize) -> bool {
    size >= LO.load(Ordering::Relaxed) && size <= HI.load(Ordering::Relaxed)
}

#[inline]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Walk the frame-pointer chain. The kernel is built with `-Cforce-frame-pointers=yes`
/// (`src/kernel/.cargo/config.toml`), so this is exact rather than heuristic -- and unlike
/// `backtracer_core` it neither allocates nor reads DWARF.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn capture(ips: &mut [u64; NR_IPS]) {
    let mut fp: u64;
    unsafe {
        core::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    for slot in ips.iter_mut() {
        // Kernel half, 8-aligned, and far enough from the top that reading [fp+8] cannot wrap.
        if fp < 0xffff_8000_0000_0000 || fp & 7 != 0 || fp > u64::MAX - 16 {
            break;
        }
        let ret = unsafe { core::ptr::read_volatile((fp + 8) as *const u64) };
        let next = unsafe { core::ptr::read_volatile(fp as *const u64) };
        if ret < 0xffff_8000_0000_0000 {
            break;
        }
        *slot = ret;
        // Frames march up the stack. A chain that does not is garbage, not a deeper frame.
        if next <= fp {
            break;
        }
        fp = next;
    }
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(never)]
fn capture(_ips: &mut [u64; NR_IPS]) {}

#[inline]
pub fn record_alloc(ptr: *mut u8, size: usize) {
    if !enabled() || !in_range(size) || ptr.is_null() {
        return;
    }
    let mut ips = [0u64; NR_IPS];
    capture(&mut ips);
    crate::interrupt::with_disabled(|| {
        let _g = Guard::acquire();
        let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
        let idx = match t.alloc_slot() {
            Some(i) => i,
            None => {
                t.overflow += 1;
                return;
            }
        };
        let b = hash(ptr as u64);
        t.slots[idx] = Slot {
            ptr: ptr as u64,
            size: size as u32,
            next: t.buckets[b],
            seq: t.inserted,
            ips,
        };
        t.buckets[b] = idx as u32 + 1;
        t.live += 1;
        t.inserted += 1;
    });
}

#[inline]
pub fn record_free(ptr: *mut u8, size: usize) {
    if !enabled() || !in_range(size) || ptr.is_null() {
        return;
    }
    crate::interrupt::with_disabled(|| {
        let _g = Guard::acquire();
        let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
        if t.remove(ptr as u64) {
            t.live -= 1;
            t.removed += 1;
        } else {
            // Allocated before the arm, or lost to overflow. Expected, and counted so a dump can
            // say how much of its own view it is missing.
            t.free_miss += 1;
        }
    });
}

impl Table {
    fn alloc_slot(&mut self) -> Option<usize> {
        if self.free != 0 {
            let idx = self.free as usize - 1;
            self.free = self.slots[idx].next;
            return Some(idx);
        }
        if self.bump < NR_SLOTS {
            let idx = self.bump;
            self.bump += 1;
            return Some(idx);
        }
        None
    }

    fn remove(&mut self, ptr: u64) -> bool {
        let b = hash(ptr);
        let mut cur = self.buckets[b];
        let mut prev: u32 = 0;
        while cur != 0 {
            let idx = cur as usize - 1;
            let next = self.slots[idx].next;
            if self.slots[idx].ptr == ptr {
                if prev == 0 {
                    self.buckets[b] = next;
                } else {
                    self.slots[prev as usize - 1].next = next;
                }
                self.slots[idx] = EMPTY_SLOT;
                self.slots[idx].next = self.free;
                self.free = idx as u32 + 1;
                return true;
            }
            prev = cur;
            cur = next;
        }
        false
    }

    fn reset(&mut self) {
        self.buckets = [0; NR_BUCKETS];
        self.bump = 0;
        self.free = 0;
        self.live = 0;
        self.inserted = 0;
        self.removed = 0;
        self.overflow = 0;
        self.free_miss = 0;
    }
}

pub fn control(ctl: &mut KallocTrackCtl) {
    match ctl.cmd {
        KALLOC_TRACK_ARM => {
            ON.store(false, Ordering::SeqCst);
            crate::interrupt::with_disabled(|| {
                let _g = Guard::acquire();
                let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
                t.reset();
            });
            LO.store(ctl.lo as usize, Ordering::SeqCst);
            HI.store(ctl.hi as usize, Ordering::SeqCst);
            let generation = ARM_GEN.fetch_add(1, Ordering::SeqCst) + 1;
            ON.store(true, Ordering::SeqCst);
            logln!(
                "[kalloc-track] armed gen={} range={}..={} slots={}",
                generation,
                ctl.lo,
                ctl.hi,
                NR_SLOTS
            );
        }
        KALLOC_TRACK_OFF => {
            ON.store(false, Ordering::SeqCst);
            logln!("[kalloc-track] off");
        }
        KALLOC_TRACK_DUMP => dump(),
        _ => {}
    }
    fill(ctl);
}

fn fill(ctl: &mut KallocTrackCtl) {
    // A racing alloc/free can move these by one or two; they are read under the lock so they are at
    // least mutually consistent.
    crate::interrupt::with_disabled(|| {
        let _g = Guard::acquire();
        let t = unsafe { &*core::ptr::addr_of!(TABLE) };
        ctl.live = t.live;
        ctl.inserted = t.inserted;
        ctl.removed = t.removed;
        ctl.overflow = t.overflow;
        ctl.free_miss = t.free_miss;
    });
}

/// Print the live set, aggregated by call-site chain, plus the first bytes of a few live blocks.
///
/// Runs on the syscall path with the tracker *disabled* for the duration: printing takes the
/// console lock and formatting may allocate, so recording must not be live underneath it. That
/// costs the alloc/free traffic concurrent with the dump, which is why the caller quiesces first.
fn dump() {
    let was_on = ON.swap(false, Ordering::SeqCst);

    // (ips, count, one example block's address and size). Fixed-size and on the stack: the dump
    // runs on a syscall, but it must still not allocate, because the thing it is measuring is the
    // allocator.
    // (ips, count, example ptr, size, oldest seq, newest seq)
    let mut sites: [([u64; NR_IPS], u64, u64, u32, u64, u64); NR_SITES] =
        [([0; NR_IPS], 0, 0, 0, 0, 0); NR_SITES];
    let mut nr_sites = 0usize;
    let mut site_overflow = 0u64;

    // Snapshot under the lock; print outside it.
    let (live, inserted, removed, overflow, free_miss) = crate::interrupt::with_disabled(|| {
        let _g = Guard::acquire();
        let t = unsafe { &*core::ptr::addr_of!(TABLE) };
        for slot in t.slots.iter().take(t.bump) {
            if slot.ptr == 0 {
                continue;
            }
            let mut found = false;
            for site in sites.iter_mut().take(nr_sites) {
                if site.0 == slot.ips {
                    site.1 += 1;
                    site.4 = site.4.min(slot.seq);
                    site.5 = site.5.max(slot.seq);
                    found = true;
                    break;
                }
            }
            if !found {
                if nr_sites < NR_SITES {
                    sites[nr_sites] = (slot.ips, 1, slot.ptr, slot.size, slot.seq, slot.seq);
                    nr_sites += 1;
                } else {
                    site_overflow += 1;
                }
            }
        }
        (t.live, t.inserted, t.removed, t.overflow, t.free_miss)
    });

    emerglogln!(
        "KALLOC-TRACK-TOTAL live={} inserted={} removed={} overflow={} free_miss={} sites={} site_overflow={}",
        live,
        inserted,
        removed,
        overflow,
        free_miss,
        nr_sites,
        site_overflow
    );
    // Biggest chain first: one mechanism that fails to balance should dominate, and a long tail of
    // ones is the ordinary live set of a class that is simply in use.
    for _ in 0..nr_sites {
        let mut best = usize::MAX;
        for (i, s) in sites.iter().enumerate().take(nr_sites) {
            if s.1 > 0 && (best == usize::MAX || s.1 > sites[best].1) {
                best = i;
            }
        }
        if best == usize::MAX {
            break;
        }
        let (ips, count, example, size, oldest, newest) = sites[best];
        sites[best].1 = 0;
        // `oldest`/`newest` against `inserted` on the TOTAL line is the read: a site whose live
        // blocks start near seq 0 has been retaining since the window opened; one whose oldest is
        // near `inserted` is simply the current generation of something that turns over.
        emerglogln!(
            "KALLOC-TRACK-SITE count={} size={} oldest={} newest={} ips={:#x},{:#x},{:#x},{:#x},{:#x},{:#x}",
            count,
            size,
            oldest,
            newest,
            ips[0],
            ips[1],
            ips[2],
            ips[3],
            ips[4],
            ips[5]
        );
        // Contents identify a struct as often as a backtrace does, and cost nothing at record time:
        // a 128-bit ObjID, an Arc's two refcounts, a kernel pointer all look like themselves. Read
        // outside the lock, so the bytes are a sample rather than a guarantee -- the block is still
        // allocated, but nothing stops its owner from writing it while this prints.
        let n = (size as usize).min(NR_DUMP_BYTES);
        let mut buf = [0u8; NR_DUMP_BYTES * 2];
        let bytes = unsafe { core::slice::from_raw_parts(example as usize as *const u8, n) };
        for (i, b) in bytes.iter().enumerate() {
            buf[i * 2] = HEX[(*b >> 4) as usize];
            buf[i * 2 + 1] = HEX[(*b & 0xf) as usize];
        }
        emerglogln!(
            "KALLOC-TRACK-BLOCK ptr={:#x} size={} bytes={}",
            example,
            size,
            core::str::from_utf8(&buf[..n * 2]).unwrap_or("?")
        );
    }

    ON.store(was_on, Ordering::SeqCst);
}

const HEX: &[u8; 16] = b"0123456789abcdef";
