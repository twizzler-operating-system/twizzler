use std::{
    alloc::GlobalAlloc,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        OnceLock,
    },
};

use monitor_api::{RuntimeThreadControl, THREAD_STARTED};
use twizzler_abi::{object::ObjID, syscall::sys_thread_gettls};

use super::{ReferenceRuntime, RuntimeState};

mod ferroc;
mod talc;

pub use talc::{LocalAllocator, LOCAL_ALLOCATOR};

static COMP_NAME: OnceLock<String> = OnceLock::new();
static COMP_NAME_READY: AtomicBool = AtomicBool::new(false);

#[thread_local]
static COMP_NAME_SKIP: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
#[allow(unused)]
fn print_comp_name(layout: std::alloc::Layout, is_free: bool) {
    return;
    if sys_thread_gettls() == 0 {
        return;
    }
    if !COMP_NAME_SKIP.load(Ordering::SeqCst) {
        COMP_NAME_SKIP.store(true, Ordering::SeqCst);
        let comp_name = if COMP_NAME_READY.swap(true, Ordering::SeqCst) {
            COMP_NAME.get()
        } else {
            let comp = monitor_api::CompartmentHandle::current();
            if let Ok(raw) = monitor_api::monitor_rt_get_compartment_info(None) {
                if raw.name_len == 6 {
                    let info = comp.info().unwrap();
                    let name = info.name.clone();
                    std::mem::forget(info);
                    Some(COMP_NAME.get_or_init(|| name))
                } else {
                    None
                }
            } else {
                None
            }
        };

        if comp_name.is_some_and(|s| s.as_str() == "naming") {
            twizzler_abi::klog_println!(
                "{:?}: alloc: {} bytes, align = {}",
                comp_name,
                layout.size(),
                layout.align()
            );
            if !is_free {
                let b = std::backtrace::Backtrace::force_capture();
                for frame in b.frames().iter().take(7).enumerate() {
                    twizzler_abi::klog_println!("frame: {:?}", frame);
                }
            }
        }
        COMP_NAME_SKIP.store(false, Ordering::SeqCst);
    }
}

/// DIAG (Mode L), diagnostic only -- set to `usize::MAX` to disable and restore normal routing.
///
/// ferroc classes a request as `Large` above `MEDIUM_MAX` (32 KiB) and up to `LARGE_MAX`
/// (~1.875 MiB), packing it into a 4 MiB slab and finding a block's owning slab by masking the
/// pointer to `SLAB_SIZE`. memhog's 1 MiB chunks land squarely on that path. Routing everything
/// above `MEDIUM_MAX` to talc instead bisects the allocator: if Mode L survives, ferroc's large
/// path is not responsible. Size-based so `dealloc` routes identically without needing to know
/// where a pointer came from.
/// Currently **disabled** (normal ferroc routing). Set to `32 << 10` to re-run the bisect. Result
/// on record: Mode L still reproduces with large allocations served by talc, so ferroc's large
/// path is not responsible.
const DIAG_TALC_ABOVE: usize = usize::MAX;

fn try_switch_allocator_is_done() -> bool {
    static SWITCHED: AtomicU32 = AtomicU32::new(0);
    if SWITCHED.load(Ordering::Acquire) == 2 {
        return true;
    }
    if SWITCHED.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst) == Ok(0) {
        LOCAL_ALLOCATOR.freeze_early_allocs();
        SWITCHED.store(2, Ordering::Release);
        true
    } else {
        false
    }
}

unsafe impl GlobalAlloc for ReferenceRuntime {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let tls =
            unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() };
        if !self.state().contains(RuntimeState::READY)
            || self.state().contains(RuntimeState::IS_MONITOR)
            || tls.is_null()
        {
            let r = LOCAL_ALLOCATOR.alloc_early(layout);
            census::on_alloc(layout.size(), census::B_EARLY_COLD);
            // The tracker's other hooks sit on the ferroc tail, which the monitor never reaches --
            // `IS_MONITOR` routes every one of its allocations here. Without this the tracker
            // reports `live=0 inserted=0` for the monitor, which reads as "nothing retained".
            census::track::on_alloc(r, layout.size());
            return r;
        }

        if !try_switch_allocator_is_done() {
            let r = LOCAL_ALLOCATOR.alloc_early(layout);
            census::on_alloc(layout.size(), census::B_EARLY_COLD);
            return r;
        }

        // Reuse the control block fetched and null-checked above rather than reading the thread
        // pointer a second time: this is the hottest path in the runtime, and a null block has
        // already been routed to the early allocator, which is the right answer for a thread with
        // no usable TLS (not yet installed, or freed underneath us).
        let ts = unsafe { (*tls).runtime_data.flags.load(Ordering::SeqCst) & THREAD_STARTED != 0 };
        if !ts {
            // TODO: this leaks the stuff that is allocated in libc's TLS
            let r = LOCAL_ALLOCATOR.alloc_early(layout);
            census::on_alloc(layout.size(), census::B_EARLY_NOTS);
            return r;
        }

        if layout.size() > DIAG_TALC_ABOVE {
            let r = LOCAL_ALLOCATOR.alloc(layout);
            census::on_alloc(layout.size(), census::B_TALC);
            return r;
        }

        census::on_alloc(layout.size(), census::B_FERROC);
        print_comp_name(layout, false);
        //let start_time = Instant::now();
        let r = ferroc::TwzFerroc
            .allocate(layout)
            .map(|nn| nn.as_ptr())
            .unwrap_or(core::ptr::null_mut())
            .cast::<u8>();
        census::track::on_alloc(r, layout.size());

        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let tls =
            unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() };
        if !self.state().contains(RuntimeState::READY)
            || self.state().contains(RuntimeState::IS_MONITOR)
            || tls.is_null()
        {
            census::on_alloc(layout.size(), census::B_EARLY_COLD);
            return LOCAL_ALLOCATOR.alloc_zeroed_early(layout);
        }

        if !try_switch_allocator_is_done() {
            census::on_alloc(layout.size(), census::B_EARLY_COLD);
            return LOCAL_ALLOCATOR.alloc_zeroed_early(layout);
        }

        // Reuse the control block fetched and null-checked above rather than reading the thread
        // pointer a second time: this is the hottest path in the runtime, and a null block has
        // already been routed to the early allocator, which is the right answer for a thread with
        // no usable TLS (not yet installed, or freed underneath us).
        let ts = unsafe { (*tls).runtime_data.flags.load(Ordering::SeqCst) & THREAD_STARTED != 0 };
        if !ts {
            // TODO: this leaks the stuff that is allocated in libc's TLS
            let r = LOCAL_ALLOCATOR.alloc_zeroed_early(layout);
            census::on_alloc(layout.size(), census::B_EARLY_NOTS);
            return r;
        }

        if layout.size() > DIAG_TALC_ABOVE {
            let r = LOCAL_ALLOCATOR.alloc_zeroed(layout);
            census::on_alloc(layout.size(), census::B_TALC);
            return r;
        }

        census::on_alloc(layout.size(), census::B_FERROC);
        print_comp_name(layout, false);
        //let start_time = Instant::now();
        let r = ferroc::TwzFerroc
            .allocate_zeroed(layout)
            .map(|nn| nn.as_ptr())
            .unwrap_or(core::ptr::null_mut())
            .cast::<u8>();
        census::track::on_alloc(r, layout.size());

        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if !self.state().contains(RuntimeState::READY) {
            census::on_free(layout.size(), Some(census::B_DROP_NOTREADY));
            return;
        }

        if self.state().contains(RuntimeState::IS_MONITOR) {
            // Monitor allocations all come from the early allocator, and the old
            // `LOCAL_ALLOCATOR.dealloc` here was dead code (do_dealloc drops everything while
            // early_allocs_frozen is false, which the monitor never sets) — every monitor free
            // leaked, ~360 B per incoming gate call. Free into the early talc, but only pointers
            // it actually owns: the monitor also frees bootstrap-allocator pointers (before
            // `bootstrap_alloc_slot` is registered), and feeding those to early_talc corrupts it —
            // a 10/10 debug-kvm monitor-bootstrap wedge, bisected to the unguarded form of this.
            if LOCAL_ALLOCATOR.is_ptr_early_alloc(ptr) {
                census::on_free(layout.size(), None);
                census::track::on_free(ptr, layout.size());
                return LOCAL_ALLOCATOR.dealloc_early(ptr, layout);
            }
            // Deliberately uncounted: this is a bootstrap-allocator pointer being dropped, and no
            // `B_*` branch describes it (`B_DROP_EARLYPTR` means the opposite). The population is
            // bounded by boot -- after `bootstrap_alloc_slot` is registered every monitor free
            // takes the arm above -- so it biases `net` upward by a constant, not by a per-load
            // term. Add a branch before reading `net` as an absolute.
            return;
        }

        if LOCAL_ALLOCATOR.is_ptr_early_alloc(ptr) {
            // Freed, not dropped. The pointer is provably `early_talc`'s (the guard matches its
            // slot against that talc's own object set), `early_talc` outlives the compartment
            // (`freeze_early_allocs` flips a bool; nothing releases a claimed heap object), and the
            // monitor arm above has been doing exactly this. Dropping it stranded every allocation
            // made in a zero-`THREAD_STARTED` window -- `libc_init_tcb` in
            // `cross_compartment_entry` most visibly, at 512 KiB a time, whose matching free
            // already arrives here from `__mlibc_handle_thread_exit`.
            census::on_free(layout.size(), None);
            census::track::on_free(ptr, layout.size());
            return LOCAL_ALLOCATOR.dealloc_early(ptr, layout);
        }
        let tls =
            unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() };
        if tls.is_null() {
            census::on_free(layout.size(), Some(census::B_DROP_NULLTLS));
            return;
        }

        // Reuse the control block fetched and null-checked above rather than reading the thread
        // pointer a second time: this is the hottest path in the runtime, and a null block has
        // already been routed to the early allocator, which is the right answer for a thread with
        // no usable TLS (not yet installed, or freed underneath us).
        let ts = unsafe { (*tls).runtime_data.flags.load(Ordering::SeqCst) & THREAD_STARTED != 0 };
        if !ts {
            census::on_free(layout.size(), Some(census::B_DROP_NOTS));
            return;
        }

        // Mirrors the routing in `alloc`; must stay after the early-alloc check above, since those
        // pointers are deliberately leaked rather than freed.
        if layout.size() > DIAG_TALC_ABOVE {
            census::on_free(layout.size(), None);
            return LOCAL_ALLOCATOR.dealloc(ptr, layout);
        }

        census::on_free(layout.size(), None);
        census::track::on_free(ptr, layout.size());
        if let Some(ptr) = NonNull::new(ptr) {
            //let start_time = Instant::now();
            print_comp_name(layout, true);
            ferroc::TwzFerroc.deallocate(ptr, layout);
            //let end_time = Instant::now();
            //trace_runtime_alloc(ptr.addr().into(), layout, end_time - start_time, true);
        }
    }
}

impl ReferenceRuntime {
    pub(crate) fn register_bootstrap_alloc(&self, slot: usize) {
        LOCAL_ALLOCATOR
            .bootstrap_alloc_slot
            .store(slot, Ordering::SeqCst);
    }

    pub fn get_id_from_heap_ptr(&self, ptr: *const u8) -> Option<ObjID> {
        LOCAL_ALLOCATOR.get_id_from_ptr(ptr)
    }

    pub fn heap_gc(&self) {
        //twizzler_abi::klog_println!("running heap GC");
        ferroc::TwzFerroc.collect(true);
    }
}

/// A per-size-class census of this compartment's userspace heap, and of every branch in
/// `alloc`/`dealloc` that does not reach the allocator.
///
/// The kernel side of the leak harness has `LEAKCHECK-KALLOC`, which names a *size class* for
/// kernel-heap growth; userspace had nothing equivalent, so `l7-spawn-proc`'s 34 pages/iter of
/// growth in two `note=heap` objects could be located to a compartment but not within it.
///
/// The branch counters matter as much as the classes: `alloc` routes to a bump allocator whose
/// frees are dropped on the floor whenever the thread's `THREAD_STARTED` flag is clear, and
/// `dealloc` has four separate early returns that discard a free. Growth from one of those is a
/// different bug from growth in a live-block class, and a net-bytes total alone cannot tell them
/// apart.
///
/// DIAG, and **disarmed by default**: the counting paths are compiled in but do nothing until
/// something calls `__twz_rt_diag_heap_census_arm`, which only the leak harness does. An
/// instrument that switches itself on before every measurement is how `perfmark` came to inflate
/// every bench absolute in this tree by up to 2.34x (`sysbench.md` F11) -- common-mode, so A/B
/// findings survived, which is exactly the protection that does not extend across the boundary
/// where it was introduced. A disarmed boot pays one relaxed load per alloc and per free and
/// changes no allocator behaviour; an armed boot pays two more atomic adds.
///
/// `ENABLED` is the compile-time master switch: setting it `false` folds every hook away.
pub(crate) mod census {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

    pub const ENABLED: bool = true;

    /// Set only by `__twz_rt_diag_heap_census_arm`. Read, never written, on the allocation path.
    static ARMED: AtomicBool = AtomicBool::new(false);

    #[inline(always)]
    fn armed() -> bool {
        ENABLED && ARMED.load(Relaxed)
    }

    /// Start counting. Returns the previous state so a caller can tell "armed by me" from
    /// "already armed by someone else" -- two arms of one boot must not both believe they own it.
    #[no_mangle]
    pub extern "C-unwind" fn __twz_rt_diag_heap_census_arm() -> u64 {
        ARMED.swap(true, Relaxed) as u64
    }

    /// `class_of(size)` = ceil-log2, so class `c` holds sizes in `(2^(c-1), 2^c]`.
    pub const NR_CLASSES: usize = 32;
    /// 8 branch counts followed by their 8 byte totals.
    pub const NR_BRANCH: usize = 16;

    pub const B_FERROC: usize = 0;
    pub const B_EARLY_COLD: usize = 1;
    pub const B_EARLY_NOTS: usize = 2;
    pub const B_TALC: usize = 3;
    pub const B_DROP_NOTREADY: usize = 4;
    pub const B_DROP_EARLYPTR: usize = 5;
    pub const B_DROP_NULLTLS: usize = 6;
    pub const B_DROP_NOTS: usize = 7;

    static A_CNT: [AtomicU64; NR_CLASSES] = [const { AtomicU64::new(0) }; NR_CLASSES];
    static A_BYTES: [AtomicU64; NR_CLASSES] = [const { AtomicU64::new(0) }; NR_CLASSES];
    static F_CNT: [AtomicU64; NR_CLASSES] = [const { AtomicU64::new(0) }; NR_CLASSES];
    static F_BYTES: [AtomicU64; NR_CLASSES] = [const { AtomicU64::new(0) }; NR_CLASSES];
    static BRANCH: [AtomicU64; NR_BRANCH] = [const { AtomicU64::new(0) }; NR_BRANCH];

    #[inline(always)]
    pub fn class_of(size: usize) -> usize {
        if size <= 1 {
            0
        } else {
            ((usize::BITS - (size - 1).leading_zeros()) as usize).min(NR_CLASSES - 1)
        }
    }

    #[inline(always)]
    pub fn on_alloc(size: usize, branch: usize) {
        if !armed() {
            return;
        }
        let c = class_of(size);
        A_CNT[c].fetch_add(1, Relaxed);
        A_BYTES[c].fetch_add(size as u64, Relaxed);
        BRANCH[branch].fetch_add(1, Relaxed);
        BRANCH[branch + 8].fetch_add(size as u64, Relaxed);
    }

    #[inline(always)]
    pub fn on_free(size: usize, branch: Option<usize>) {
        if !armed() {
            return;
        }
        match branch {
            // A discarded free is not a free: count it on the branch, never in `F_*`, so that
            // `net = alloc - free` stays the number of blocks the heap is still holding.
            Some(b) => {
                BRANCH[b].fetch_add(1, Relaxed);
                BRANCH[b + 8].fetch_add(size as u64, Relaxed);
            }
            None => {
                let c = class_of(size);
                F_CNT[c].fetch_add(1, Relaxed);
                F_BYTES[c].fetch_add(size as u64, Relaxed);
            }
        }
    }

    /// A live-block table for one size range: which blocks were allocated and not freed.
    ///
    /// A size class names *what* is retained; it cannot name *who* allocated it. The kernel side of
    /// this harness answers that with `kalloc_track`, and the obvious userspace analogue -- capture
    /// a backtrace in the allocator -- is the one thing that must not be done here: it allocates,
    /// from inside the allocator. (The kernel's `--kalloc-trap` is interlocked off for exactly that
    /// reason.) So this records addresses only, and identification comes from *reading the retained
    /// bytes* afterwards: a retained `String` shows its text, a `Vec<binding_info>` shows object
    /// ids, a boxed struct shows its first field. That is usually enough to name the allocation
    /// site, and it costs no unwinding and no allocation.
    ///
    /// Open-addressed, fixed capacity, lock-free (CAS on each slot), and disarmed by default.
    /// Overflow is counted rather than silently dropped: a full table and an empty one must not
    /// serialize to the same dump.
    pub mod track {
        use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

        pub const CAP: usize = 8192;
        const PROBE: usize = 12;
        const EMPTY: usize = 0;
        const TOMB: usize = 1;

        static SLOT_PTR: [AtomicUsize; CAP] = [const { AtomicUsize::new(EMPTY) }; CAP];
        static SLOT_SZ: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
        static LO: AtomicUsize = AtomicUsize::new(1);
        static HI: AtomicUsize = AtomicUsize::new(0);
        /// `[inserted, removed, overflow, free_miss]`.
        static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

        #[inline(always)]
        fn in_range(size: usize) -> bool {
            let lo = LO.load(Relaxed);
            let hi = HI.load(Relaxed);
            lo <= hi && size >= lo && size <= hi
        }

        #[inline(always)]
        fn home(ptr: usize) -> usize {
            // Heap pointers are 16-byte aligned and clustered; mix the high bits down so that a
            // run of adjacent blocks does not land in one probe window.
            let mut x = (ptr >> 4) as u64;
            x ^= x >> 29;
            x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x ^= x >> 32;
            (x as usize) % CAP
        }

        #[inline(always)]
        pub fn on_alloc(ptr: *mut u8, size: usize) {
            // Behind the census's own arm, so a disarmed boot pays exactly one relaxed load on
            // each of alloc and free -- the number quoted for the disarmed cost, not one plus
            // however many gates were added later.
            if !super::armed() {
                return;
            }
            if ptr.is_null() || !in_range(size) {
                return;
            }
            let p = ptr as usize;
            let h = home(p);
            for i in 0..PROBE {
                let s = (h + i) % CAP;
                let cur = SLOT_PTR[s].load(Relaxed);
                if cur == EMPTY || cur == TOMB {
                    if SLOT_PTR[s]
                        .compare_exchange(cur, p, AcqRel, Relaxed)
                        .is_ok()
                    {
                        SLOT_SZ[s].store(size, Release);
                        STATS[0].fetch_add(1, Relaxed);
                        return;
                    }
                }
            }
            STATS[2].fetch_add(1, Relaxed);
        }

        #[inline(always)]
        pub fn on_free(ptr: *mut u8, size: usize) {
            if !super::armed() {
                return;
            }
            if ptr.is_null() || !in_range(size) {
                return;
            }
            let p = ptr as usize;
            let h = home(p);
            for i in 0..PROBE {
                let s = (h + i) % CAP;
                if SLOT_PTR[s].load(Acquire) == p {
                    if SLOT_PTR[s]
                        .compare_exchange(p, TOMB, AcqRel, Relaxed)
                        .is_ok()
                    {
                        STATS[1].fetch_add(1, Relaxed);
                        return;
                    }
                }
            }
            // Freed a block this table never held: allocated before arming, or lost to overflow.
            STATS[3].fetch_add(1, Relaxed);
        }

        /// Arm over `[lo, hi]` and clear the table. `lo > hi` disarms.
        #[no_mangle]
        pub extern "C-unwind" fn __twz_rt_diag_heap_track_arm(lo: usize, hi: usize) {
            LO.store(usize::MAX, Relaxed);
            HI.store(0, Relaxed);
            for s in 0..CAP {
                SLOT_PTR[s].store(EMPTY, Relaxed);
                SLOT_SZ[s].store(0, Relaxed);
            }
            for c in STATS.iter() {
                c.store(0, Relaxed);
            }
            LO.store(lo, Relaxed);
            HI.store(hi, Release);
        }

        /// Write `[ptr, size]` for every live block, then the four stats and a truncation count.
        /// Returns words written.
        ///
        /// The caller's buffer is smaller than `CAP`, so it can fill before the table is walked.
        /// That is reported rather than silently dropped -- a dump that stopped early and a table
        /// that held exactly that many blocks would otherwise read identically.
        #[no_mangle]
        pub extern "C-unwind" fn __twz_rt_diag_heap_track_dump(out: *mut u64, n: usize) -> usize {
            if out.is_null() || n < 5 {
                return 0;
            }
            let mut w = 0usize;
            let mut truncated = 0u64;
            for s in 0..CAP {
                let p = SLOT_PTR[s].load(Acquire);
                if p == EMPTY || p == TOMB {
                    continue;
                }
                if w + 2 + 5 > n {
                    truncated += 1;
                    continue;
                }
                unsafe {
                    *out.add(w) = p as u64;
                    *out.add(w + 1) = SLOT_SZ[s].load(Acquire) as u64;
                }
                w += 2;
            }
            for (i, c) in STATS.iter().enumerate() {
                unsafe { *out.add(w + i) = c.load(Relaxed) };
            }
            unsafe { *out.add(w + 4) = truncated };
            w + 5
        }
    }

    /// Snapshot: `NR_BRANCH` branch counters, then `NR_CLASSES` groups of
    /// `[alloc_count, alloc_bytes, free_count, free_bytes]`. Returns the number of words written,
    /// or 0 if the census is not armed -- an all-zero table would otherwise read as "nothing was
    /// allocated" when it means "nothing was counted".
    #[no_mangle]
    pub extern "C-unwind" fn __twz_rt_diag_heap_census(out: *mut u64, n: usize) -> usize {
        let need = NR_BRANCH + NR_CLASSES * 4;
        if out.is_null() || n < need || !armed() {
            return 0;
        }
        let mut w = |i: usize, v: u64| unsafe { *out.add(i) = v };
        for i in 0..NR_BRANCH {
            w(i, BRANCH[i].load(Relaxed));
        }
        for c in 0..NR_CLASSES {
            let b = NR_BRANCH + c * 4;
            w(b, A_CNT[c].load(Relaxed));
            w(b + 1, A_BYTES[c].load(Relaxed));
            w(b + 2, F_CNT[c].load(Relaxed));
            w(b + 3, F_BYTES[c].load(Relaxed));
        }
        need
    }
}
