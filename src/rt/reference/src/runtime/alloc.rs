use std::{
    alloc::GlobalAlloc,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        OnceLock,
    },
};

use monitor_api::{RuntimeThreadControl, THREAD_STARTED};
use twizzler_abi::{
    object::{ObjID, MAX_SIZE},
    syscall::sys_thread_gettls,
};

use super::{ReferenceRuntime, RuntimeState};

pub(crate) mod anon;
pub(crate) mod ferroc;
pub(crate) mod talc;

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

/// Route the monitor's post-ready allocations through ferroc like any other compartment's,
/// instead of pinning every monitor allocation to the early talc allocator -- one simple_mutex
/// for the whole process, held by every gate call's allocations.
///
/// The exclusion was constructional, not fundamental: the monitor's gate-borrowed threads get
/// TLS and THREAD_STARTED from the same `cross_compartment_entry` machinery every service
/// compartment's gate handlers already run ferroc on; its background threads come through the
/// ordinary spawn trampoline; `__monitor_ready` runs before any of them exist; and ferroc's
/// base-chunk growth reaches `create_and_map`'s monitor arm (direct `sys_object_map`, no gate
/// recursion) exactly as early-talc growth does today. What the exclusion *was* load-bearing
/// for is bootstrap-allocator pointers, which belong to neither talc nor ferroc -- the slot
/// guard in `dealloc` covers those on this path.
///
/// Arm selector, one tree state builds both; `false` is bit-for-bit the pre-monitor-ferroc
/// behavior. Validated on (tags monferroc/monferroc2, 2026-08-26): 2x release-kvm-smp4 and 2x
/// debug-kvm-smp4, 58/58 tests each, compartment_spawn bench + full suite, debug assertions
/// live; release spawn flat against the immediately-prior baseline (2.65-2.70 vs 2.67 ms/iter).
/// Transcripts name their arm via the `[monitor] alloc tunables` boot line. Not yet measured:
/// monitor heap retention under dead gate-thread churn (a leak-harness pass), and the paired-A/B
/// win, deferred to a quiescent tree.
pub const MONITOR_FERROC: bool = true;

/// Whether this instance routes to the early allocator unconditionally; see [`MONITOR_FERROC`].
#[inline]
fn monitor_stays_early(rt: &ReferenceRuntime) -> bool {
    !MONITOR_FERROC && rt.state().contains(RuntimeState::IS_MONITOR)
}

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

/// Census of `realloc` copy traffic.
///
/// Neither runtime overrides `GlobalAlloc::realloc`, so the default is in force: allocate, copy
/// the whole live range, free. There is no in-place grow path at all. glibc, by contrast, extends
/// a large chunk with `mremap` -- page-table work, not a copy -- so any Rust code that grows a
/// buffer by doubling pays O(n) here and ~O(1) on Linux. The suspected victim is
/// `generate_crate_metadata`, whose encoder streams into a growing buffer and is the one pass
/// slower on Twizzler in every configuration.
///
/// Counting before fixing: an in-place grow needs allocator support, and it is only worth adding
/// if the copy volume is actually large.
/// DWARF-based allocation-site recorder: which call sites make the allocations, keyed on the whole
/// stack rather than on one frame.
///
/// Replaces an `rbp` walk that terminated inside std's alloc layer -- it could name
/// `RawVec::finish_grow` but never the caller that grew -- and that the rust `MayOmit`
/// frame-pointer work will only shorten further. `.eh_frame` does not depend on frame pointers.
///
/// Records raw return addresses only; symbolization is offline (`addr2line` against the
/// compartment's ELF, in-object offset = `ip % (1 << 30)`, since slots are 1 GiB and only the slot
/// differs between compartments).
pub(crate) mod sites {
    use std::{
        ffi::c_void,
        fmt::Write as _,
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
    };

    /// Off by default: `_Unwind_Backtrace` walks `dl_iterate_phdr` on every call, which costs
    /// orders of magnitude more than the allocation it is recording.
    pub const REPORT_ON: bool = false;
    /// Sample one in every N qualifying allocations. PRIME on purpose: a round stride aliases
    /// with any periodic allocation pattern -- a loop that allocates on a fixed-length cycle would
    /// be either always sampled or never sampled, which silently deletes or manufactures a site.
    /// A prime stride shares no factor with such a cycle, so every site is reachable.
    ///
    /// Reported counts are raw sample counts; multiply by this to estimate true volume.
    const TARGET_CRATE: &str = "syn";
    const SAMPLE_EVERY: u64 = 101;
    static TICK: AtomicU64 = AtomicU64::new(0);

    /// Stop after this many recorded stacks and dump immediately. Not a perf concession: at full
    /// rate a single crate does not finish inside a 30-minute guest timeout, so without a cap the
    /// run yields nothing at all. A prefix of one rustc's allocations still names the dominant
    /// sites; it is biased toward early compilation and must be read that way.
    const MAX_SAMPLES: u64 = 800;
    /// The twizzler-vs-linux allocation gap was measured at and above this size.
    const MIN_SIZE: usize = 512;
    const DEPTH: usize = 8;
    // Keep the whole instrument well under 100 KiB of BSS: at 16384x12 it was ~1.9 MiB per
    // compartment, which starved the pager and stalled the boot before the target ever ran.
    const NENT: usize = 1024;

    // Resolved from `libunwind.so`, which is already in the initrd and already supplies this
    // compartment's `_Unwind_Resume` -- so this adds no dependency and no link change.
    extern "C" {
        fn _Unwind_Backtrace(
            cb: extern "C" fn(*mut c_void, *mut c_void) -> i32,
            arg: *mut c_void,
        ) -> i32;
        fn _Unwind_GetIP(ctx: *mut c_void) -> usize;
    }
    /// `_URC_NO_REASON` / `_URC_END_OF_STACK`.
    const URC_CONTINUE: i32 = 0;
    const URC_STOP: i32 = 5;

    static ARMED: AtomicBool = AtomicBool::new(false);
    /// Cleared by the sample cap. Counting is a free atomic; only the UNWIND is expensive, so the
    /// cap must stop unwinding WITHOUT stopping the counter -- otherwise `recs` reports where the
    /// cap tripped rather than the program's true allocation count, which is not a measurement.
    static SAMPLING: AtomicBool = AtomicBool::new(false);
    /// The target's crate name, for labelling the report -- `argv[0]` is empty on this system.
    static WHO: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    static HASH: [AtomicU64; NENT] = [const { AtomicU64::new(0) }; NENT];
    static CNT: [AtomicU64; NENT] = [const { AtomicU64::new(0) }; NENT];
    static BYTES: [AtomicU64; NENT] = [const { AtomicU64::new(0) }; NENT];
    static FRAMES: [[AtomicUsize; DEPTH]; NENT] =
        [const { [const { AtomicUsize::new(0) }; DEPTH] }; NENT];
    /// Unwind outcome counters. A silent run must say whether `record` never ran, ran and the
    /// unwinder yielded no frame, or ran fine and there was simply nothing to record.
    static RECS: AtomicU64 = AtomicU64::new(0);
    static NOENT: AtomicU64 = AtomicU64::new(0);
    static CALLS: AtomicU64 = AtomicU64::new(0);
    static ZERO: AtomicU64 = AtomicU64::new(0);
    static OK: AtomicU64 = AtomicU64::new(0);
    static LASTRC: AtomicU64 = AtomicU64::new(0);

    /// Samples dropped because the table was full. Saturation must never be silent: the 512-entry
    /// predecessor dropped 18,061 on the busiest compartment and said so only by accident.
    static OVERFLOW: AtomicU64 = AtomicU64::new(0);

    /// Reentrancy guard keyed by kernel thread id rather than TLS: the unwinder itself allocates
    /// (`dl_iterate_phdr` -> monitor gate -> `name.clone()`), so `record` re-enters the allocator,
    /// and a TLS read is not safe on every path that reaches here.
    static BUSY: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];

    struct Busy(u64);
    impl Drop for Busy {
        fn drop(&mut self) {
            for s in BUSY.iter() {
                if s.load(Relaxed) == self.0 {
                    s.store(0, Relaxed);
                    return;
                }
            }
        }
    }

    fn enter() -> Option<Busy> {
        let tid = twizzler_abi::syscall::sys_thread_self_id().raw() as u64;
        if tid == 0 {
            return None;
        }
        for s in BUSY.iter() {
            if s.load(Relaxed) == tid {
                return None;
            }
        }
        for s in BUSY.iter() {
            if s.compare_exchange(0, tid, Relaxed, Relaxed).is_ok() {
                return Some(Busy(tid));
            }
        }
        None
    }

    struct Collect {
        n: usize,
        ip: [usize; DEPTH],
    }

    extern "C" fn trace_cb(ctx: *mut c_void, arg: *mut c_void) -> i32 {
        // SAFETY: `arg` is the `&mut Collect` handed to `_Unwind_Backtrace` below, which outlives
        // the unwind, and the unwinder calls this on one thread at a time.
        let c = unsafe { &mut *(arg as *mut Collect) };
        if c.n >= DEPTH {
            return URC_STOP;
        }
        let ip = unsafe { _Unwind_GetIP(ctx) };
        if ip == 0 {
            return URC_STOP;
        }
        c.ip[c.n] = ip;
        c.n += 1;
        URC_CONTINUE
    }

    /// Only this program records. Two reasons, both learned the hard way:
    ///
    /// The monitor MUST NOT record. It allocates while holding its own locks, and the unwinder
    /// re-enters it (`_Unwind_Backtrace` -> `dl_iterate_phdr` -> monitor gate), so recording there
    /// deadlocks the monitor against itself during bootstrap -- observed as `MONLOCK: held by
    /// thread .. at mon/thread.rs:394` repeating forever, with the boot never completing.

    pub fn arm(is_monitor: bool) {
        if !REPORT_ON || is_monitor {
            return;
        }
        // Identify the target by an env var cargo sets on every rustc it spawns. `argv[0]` is
        // empty here (measured: `who=[]` on every compartment) so it cannot be used, and arming
        // every compartment stalls the boot: the long-lived servers then unwind on every
        // allocation and never exit, so they never report the counters that would have shown it.
        // One rustc, not all of them. Measured: ~0.5 s per unwind (`dl_iterate_phdr` through a
        // monitor gate), so even at stride 101 arming every crate leaves a ~100x slowdown and the
        // guest times out three crates in. `syn` is the heaviest compile in this build, so it is
        // the one worth the budget; everything else runs at full speed.
        let who = std::env::var("CARGO_CRATE_NAME").unwrap_or_default();
        if who != TARGET_CRATE {
            return;
        }
        if let Ok(mut w) = WHO.lock() {
            *w = who;
        }
        ARMED.store(true, Relaxed);
        SAMPLING.store(true, Relaxed);
    }

    pub fn record(size: usize) {
        if !REPORT_ON || size < MIN_SIZE || !ARMED.load(Relaxed) {
            return;
        }
        RECS.fetch_add(1, Relaxed);
        // Sample before taking the guard: an unsampled call must cost nothing but the counter.
        if !SAMPLING.load(Relaxed) {
            return;
        }
        if SAMPLE_EVERY > 1 && TICK.fetch_add(1, Relaxed) % SAMPLE_EVERY != 0 {
            return;
        }
        let Some(_busy) = enter() else {
            NOENT.fetch_add(1, Relaxed);
            return;
        };
        let mut c = Collect {
            n: 0,
            ip: [0; DEPTH],
        };
        CALLS.fetch_add(1, Relaxed);
        let rc = unsafe { _Unwind_Backtrace(trace_cb, (&mut c) as *mut Collect as *mut c_void) };
        LASTRC.store(rc as u32 as u64, Relaxed);
        if c.n == 0 {
            // Self-disarm rather than wreck the boot. Each call walks `dl_iterate_phdr` through a
            // monitor gate, so if the unwinder never yields a frame every further call is pure
            // cost -- enough of it that the guest did not finish booting in 13 minutes. Diagnose
            // once, then get out of the way so the run still reaches the program of interest.
            if ZERO.fetch_add(1, Relaxed) >= 200 && OK.load(Relaxed) == 0 {
                ARMED.store(false, Relaxed);
            }
            return;
        }
        let nok = OK.fetch_add(1, Relaxed) + 1;
        // FNV-1a over the whole stack: the entire point is to separate callers that share a frame.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for ip in &c.ip[..c.n] {
            h ^= *ip as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        if h == 0 {
            h = 1; // 0 marks an empty slot
        }
        let mut idx = (h as usize) % NENT;
        let mut placed = false;
        for _ in 0..NENT {
            match HASH[idx].compare_exchange(0, h, Relaxed, Relaxed) {
                Ok(_) => {
                    for (slot, ip) in FRAMES[idx].iter().zip(&c.ip[..c.n]) {
                        slot.store(*ip, Relaxed);
                    }
                    placed = true;
                    break;
                }
                Err(cur) if cur == h => {
                    placed = true;
                    break;
                }
                Err(_) => idx = (idx + 1) % NENT,
            }
        }
        if placed {
            CNT[idx].fetch_add(1, Relaxed);
            BYTES[idx].fetch_add(size as u64, Relaxed);
        } else {
            OVERFLOW.fetch_add(1, Relaxed);
        }
        if nok >= MAX_SAMPLES {
            // Stop unwinding, keep counting. The stacks are a prefix sample; `recs` must remain a
            // true total so it can be compared against another system's allocation count.
            SAMPLING.store(false, Relaxed);
        }
    }

    /// Print this compartment's security-context id alongside what it is, so a kernel fault census
    /// (which knows only `heap:<sctx>`) can be joined to human names offline.
    ///
    /// Long-lived compartments -- servers, the monitor -- never exit and so never print, which is
    /// itself the discriminator: an sctx in the census but absent here is a service, not the build.
    pub const COMPMAP_ON: bool = false;

    /// Captured at pre-main, NOT at exit: the environment is no longer readable from the exit path
    /// (measured -- `CARGO_CRATE_NAME` comes back empty there while it reads fine at pre-main).
    static IDENT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

    pub fn note_identity() {
        if !COMPMAP_ON {
            return;
        }
        // The COMPARTMENT's name, from the monitor -- not the crate being compiled. `syn` and
        // `tracing_attributes` are what a rustc compartment is working on, not what it is; the
        // servers have no crate at all and were showing up as unnamed. The crate name is kept as a
        // qualifier because a build runs ~30 rustc compartments and the crate is what tells them
        // apart.
        let comp = monitor_api::CompartmentHandle::current();
        let name = comp
            .info()
            .map(|i| {
                let n = i.name.clone();
                // `info()` hands back a handle whose Drop would release monitor state we are only
                // borrowing to read a name; the existing accessor in this file forgets it too.
                std::mem::forget(i);
                n
            })
            .unwrap_or_default();
        let krate = std::env::var("CARGO_CRATE_NAME").unwrap_or_default();
        if let Ok(mut i) = IDENT.lock() {
            *i = format!("{}|{}", name, krate);
        }
        // Emitted here, at pre-main, rather than only at exit: servers and the monitor never exit,
        // which is exactly why they had no name in the previous breakdown.
        emit();
    }

    fn emit() {
        let sctx = secgate::get_sctx_id().raw() as u64;
        let ident = IDENT.lock().map(|i| i.clone()).unwrap_or_default();
        let (comp, krate) = ident.split_once('|').unwrap_or(("", ""));
        secgate::statcadence::report_forced(format_args!(
            "COMPMAP sctx={:016x} comp=[{}] crate=[{}]",
            sctx, comp, krate
        ));
    }

    /// The compartment identity captured at pre-main, as `comp:crate`. Shared with MALLOCSTAT so
    /// heap volume and the kernel's per-compartment fault counts can be joined on one key.
    pub fn ident_string() -> String {
        let i = IDENT.lock().map(|i| i.clone()).unwrap_or_default();
        let (comp, krate) = i.split_once('|').unwrap_or(("", ""));
        let comp = comp.rsplit('/').next().unwrap_or("?");
        if krate.is_empty() {
            comp.to_owned()
        } else {
            format!("{comp}:{krate}")
        }
    }

    pub fn compmap() {
        if !COMPMAP_ON {
            return;
        }
        emit();
    }

    static REPORTED: AtomicBool = AtomicBool::new(false);

    pub fn report() {
        // Both `post_main_hook` and the `process::exit` path call this; whichever runs first wins,
        // so a program that exits either way is dumped exactly once.
        if !REPORT_ON || REPORTED.swap(true, Relaxed) {
            return;
        }
        // Reporting formats and therefore allocates; without this the dump records itself.
        let armed = ARMED.swap(false, Relaxed);
        SAMPLING.store(false, Relaxed);
        let (mut any, mut stacks, mut total) = (false, 0u64, 0u64);
        for i in 0..NENT {
            if HASH[i].load(Relaxed) == 0 {
                continue;
            }
            let n = CNT[i].load(Relaxed);
            if n == 0 {
                continue;
            }
            any = true;
            stacks += 1;
            total += n;
            let mut ips = String::with_capacity(DEPTH * 19);
            for f in FRAMES[i].iter() {
                let ip = f.load(Relaxed);
                if ip == 0 {
                    break;
                }
                let _ = write!(ips, " {:x}", ip);
            }
            secgate::statcadence::report_forced(format_args!(
                "ALLOCSITE n={} b={}{}",
                n,
                BYTES[i].load(Relaxed),
                ips
            ));
        }
        // Emitted unconditionally, even with nothing recorded: a silent run must distinguish
        // "this compartment never armed" from "report was never reached at all".
        let _ = any;
        let who = WHO.lock().map(|w| w.clone()).unwrap_or_default();
        secgate::statcadence::report_forced(format_args!(
            "ALLOCSITE-END who=[{}] armed={} stacks={} allocs={} overflow={} \
             sample_every={} recs={} noenter={} calls={} unwind_ok={} unwind_zero={} lastrc={}",
            who,
            armed,
            stacks,
            total,
            OVERFLOW.load(Relaxed),
            SAMPLE_EVERY,
            RECS.load(Relaxed),
            NOENT.load(Relaxed),
            CALLS.load(Relaxed),
            OK.load(Relaxed),
            ZERO.load(Relaxed),
            LASTRC.load(Relaxed)
        ));
    }
}

pub(crate) mod reallocstats {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// Off by default: prints on every compartment exit, and a build exits ~25 of them.
    pub const REPORT_ON: bool = false;

    pub static CALLS: AtomicU64 = AtomicU64::new(0);
    /// Bytes actually memcpy'd, i.e. `min(old, new)` summed.
    pub static COPIED: AtomicU64 = AtomicU64::new(0);
    pub static GROWS: AtomicU64 = AtomicU64::new(0);
    /// Copies of at least 64 KiB, and their bytes -- the ones an in-place grow would save.
    pub static BIG: AtomicU64 = AtomicU64::new(0);
    pub static BIG_COPIED: AtomicU64 = AtomicU64::new(0);
    /// Largest single copy seen.
    pub static MAX: AtomicU64 = AtomicU64::new(0);
    /// Reallocs whose new size already fits the block ferroc actually handed out -- the ones an
    /// in-place grow would turn into a no-op. Counted before the fix is switched on, because Rust
    /// grows by *doubling* and a doubling rarely fits the slack of a size-class allocator.
    pub static WOULD_FIT: AtomicU64 = AtomicU64::new(0);
    pub static WOULD_FIT_BYTES: AtomicU64 = AtomicU64::new(0);
    /// Of `WOULD_FIT`, the shrinks -- kept apart because they are the half that retains memory.
    pub static WOULD_FIT_SHRINK: AtomicU64 = AtomicU64::new(0);

    #[inline(always)]
    pub fn record(old: usize, new: usize) {
        let copied = old.min(new) as u64;
        CALLS.fetch_add(1, Relaxed);
        COPIED.fetch_add(copied, Relaxed);
        if new > old {
            GROWS.fetch_add(1, Relaxed);
        }
        if copied >= 65536 {
            BIG.fetch_add(1, Relaxed);
            BIG_COPIED.fetch_add(copied, Relaxed);
        }
        MAX.fetch_max(copied, Relaxed);
    }

    pub fn report() {
        if !REPORT_ON {
            return;
        }
        let calls = CALLS.load(Relaxed);
        if calls == 0 {
            return;
        }
        let fits = WOULD_FIT.load(Relaxed);
        secgate::statcadence::report_forced(format_args!(
            "REALLOCSTAT {} calls ({} grows), {} KiB copied, {} B mean; \
             >=64KiB: {} calls {} KiB; max {} KiB; \
             would-fit-in-place {} ({}%), saving {} KiB",
            calls,
            GROWS.load(Relaxed),
            COPIED.load(Relaxed) / 1024,
            COPIED.load(Relaxed) / calls,
            BIG.load(Relaxed),
            BIG_COPIED.load(Relaxed) / 1024,
            MAX.load(Relaxed) / 1024,
            fits,
            fits * 100 / calls,
            WOULD_FIT_BYTES.load(Relaxed) / 1024,
        ));
        secgate::statcadence::report_forced(format_args!(
            "REALLOCSHRINK {} of {} in-place hits were shrinks",
            WOULD_FIT_SHRINK.load(Relaxed),
            fits,
        ));
    }
}

/// Return the same pointer from `realloc` when the block ferroc already handed out is big enough.
///
/// Safe only because `DIAG_TALC_ABOVE` is `usize::MAX`, so every post-boot allocation goes to
/// ferroc and no size threshold can move a block between allocators when its layout changes; and
/// because ferroc's `deallocate` reads the size from slab metadata, using the passed layout only
/// for a debug assertion that the block is *at least* the requested size -- which an in-place grow
/// keeps true.
///
/// **Off because it was measured, not because it is unsafe.** Hit rate on a guest `cargo build`
/// is 13-16% of reallocs in the big compartments (96,802 of 700,328 in the largest), because Rust
/// grows by doubling and a doubling rarely fits a size class's slack. That is ~145k avoided
/// alloc+copy+free across the build, worth order 15 ms of a 24 s build -- about 0.06%. Flip it on
/// if a workload with a different growth pattern ever wants it; do not expect it to show up in a
/// compile benchmark.
///
/// Covers shrinks as well as grows -- see the `fits` split in `realloc`, which needs a different
/// rule for each because only a shrink can retain memory.
const REALLOC_IN_PLACE: bool = true;

impl ReferenceRuntime {
    /// Whether `ptr` belongs to ferroc, mirroring the routing in `dealloc`. `layout_of` is UB
    /// otherwise, and in a debug build walks into `unreachable!`.
    #[inline]
    unsafe fn ferroc_owns(&self, ptr: *mut u8) -> bool {
        if !self.state().contains(RuntimeState::READY) || monitor_stays_early(self) {
            return false;
        }
        if MONITOR_FERROC {
            let bslot = LOCAL_ALLOCATOR.bootstrap_alloc_slot.load(Ordering::SeqCst);
            if bslot != 0 && (ptr as usize) / MAX_SIZE == bslot {
                return false;
            }
        }
        if LOCAL_ALLOCATOR.is_ptr_early_alloc(ptr) {
            return false;
        }
        let tls =
            unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() };
        if tls.is_null() {
            return false;
        }
        unsafe { (*tls).runtime_data.flags.load(Ordering::SeqCst) & THREAD_STARTED != 0 }
    }
}

unsafe impl GlobalAlloc for ReferenceRuntime {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // See BIGALLOC in syms.rs: same fingerprint for Rust-side callers that never pass
        // through twz_rt_malloc. >=1GB only -- unreachable on any healthy path.
        if layout.size() >= 1 << 30 {
            twizzler_abi::klog_println!(
                "BIGALLOC: rust alloc size {:x} align {:x}",
                layout.size(),
                layout.align()
            );
        }
        let tls =
            unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() };
        if !self.state().contains(RuntimeState::READY) || monitor_stays_early(self) || tls.is_null()
        {
            let r = LOCAL_ALLOCATOR.alloc_early(layout);
            census::on_alloc(layout.size(), census::B_EARLY_COLD);
            // The tracker's other hooks sit on the ferroc tail, which the monitor never reaches
            // while [`MONITOR_FERROC`] is off -- this arm takes every one of its allocations.
            // Without this the tracker reports `live=0 inserted=0` for the monitor, which reads
            // as "nothing retained".
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
        sites::record(layout.size());

        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        if layout.size() >= 1 << 30 {
            twizzler_abi::klog_println!(
                "BIGALLOC: rust alloc_zeroed size {:x} align {:x}",
                layout.size(),
                layout.align()
            );
        }
        let tls =
            unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() };
        if !self.state().contains(RuntimeState::READY) || monitor_stays_early(self) || tls.is_null()
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
        sites::record(layout.size());

        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    /// Spelled out rather than inherited so the block ferroc actually handed out can be consulted
    /// before copying: a size-class allocator rounds up, and a grow that still fits the rounded
    /// block needs no new allocation and no copy at all.
    ///
    /// `layout_of` is UB on a pointer ferroc does not own, so [`Self::ferroc_owns`] gates it with
    /// the same checks `dealloc` uses to route.
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        if reallocstats::REPORT_ON {
            reallocstats::record(layout.size(), new_size);
        }
        if (REALLOC_IN_PLACE || reallocstats::REPORT_ON)
            && new_size != 0
            && layout.size() != 0
            && self.ferroc_owns(ptr)
        {
            let block =
                unsafe { ferroc::TwzFerroc.layout_of(core::ptr::NonNull::new_unchecked(ptr)) };
            let grow = new_size > layout.size();
            // A grow only has to fit the block already handed out; it retains nothing new. A
            // shrink does retain -- keeping the block means `shrink_to_fit` frees nothing -- so
            // shrinks keep it only down to half, the same rule ferroc's own C `realloc` uses
            // (`ports/ferroc/src/c.rs`: `(old_size / 2..old_size).contains(&new_size)`). That
            // bounds retention at 2x, the internal fragmentation the allocator already accepts
            // for any allocation this size. Below half, fall through and let the block move to a
            // smaller class, which is the entire point of shrinking.
            let fits = if grow {
                new_size <= block.size()
            } else {
                new_size >= block.size() / 2
            };
            // `>=` on align too: the block must still satisfy what the caller asked for.
            if fits && block.align() >= layout.align() {
                if reallocstats::REPORT_ON {
                    reallocstats::WOULD_FIT.fetch_add(1, Ordering::Relaxed);
                    if !grow {
                        reallocstats::WOULD_FIT_SHRINK.fetch_add(1, Ordering::Relaxed);
                    }
                    reallocstats::WOULD_FIT_BYTES
                        .fetch_add(layout.size().min(new_size) as u64, Ordering::Relaxed);
                }
                if REALLOC_IN_PLACE {
                    return ptr;
                }
            }
        }
        let new_layout = std::alloc::Layout::from_size_align_unchecked(new_size, layout.align());
        let new_ptr = self.alloc(new_layout);
        if !new_ptr.is_null() {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, core::cmp::min(layout.size(), new_size));
            self.dealloc(ptr, layout);
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if !self.state().contains(RuntimeState::READY) {
            census::on_free(layout.size(), Some(census::B_DROP_NOTREADY));
            return;
        }

        if monitor_stays_early(self) {
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

        // Bootstrap-allocator pointers (a slot is registered only in the monitor) belong to
        // neither talc nor ferroc; `LocalAllocator::dealloc` drops them for the same reason, and
        // with [`MONITOR_FERROC`] on nothing else catches them before the ferroc tail, which
        // masks a foreign pointer to a slab it does not own. Uncounted, exactly as the arm above
        // leaves them: the population is bounded by boot. Compiled out with the selector off, so
        // that arm stays bit-for-bit the pre-existing behavior.
        if MONITOR_FERROC {
            let bslot = LOCAL_ALLOCATOR.bootstrap_alloc_slot.load(Ordering::SeqCst);
            if bslot != 0 && (ptr as usize) / MAX_SIZE == bslot {
                return;
            }
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

    /// Live heap bytes and their high-water mark. Trustworthy now in a way the earlier attempt was
    /// not: that one never decremented for discarded frees, so LIVE only ever grew. `MALLOCSTAT`
    /// shows frees now match allocs to within 0.04% of count, so the running difference is real.
    static LIVE: AtomicU64 = AtomicU64::new(0);
    static PEAK: AtomicU64 = AtomicU64::new(0);

    pub fn peak_live() -> u64 {
        PEAK.load(Relaxed)
    }

    /// Live bytes right now. Reported next to PEAK as a self-check: the totals imply a specific
    /// end state (alloc bytes - free bytes), so if this does not land there, frees are escaping
    /// `on_free` and PEAK is inflated by however much leaked.
    pub fn live_now() -> u64 {
        LIVE.load(Relaxed)
    }

    /// Frees whose size exceeded LIVE -- i.e. a free with no matching counted alloc. Nonzero means
    /// the two sides are not paired and neither LIVE nor PEAK can be believed.
    pub static UNDERFLOW: AtomicU64 = AtomicU64::new(0);

    #[inline(always)]
    pub fn on_alloc(size: usize, branch: usize) {
        if !armed() {
            return;
        }
        let live = LIVE.fetch_add(size as u64, Relaxed) + size as u64;
        PEAK.fetch_max(live, Relaxed);
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
        // Decremented for EVERY free, discarded or not: the memory is gone from the program's view
        // either way, and only counting the accounted ones is exactly what made the previous PEAK
        // meaningless (it could only ever grow). Clamped so a race cannot wrap it below zero.
        let cur = LIVE.load(Relaxed);
        if (size as u64) > cur {
            UNDERFLOW.fetch_add(1, Relaxed);
        }
        LIVE.fetch_sub((size as u64).min(cur), Relaxed);
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

    /// Whole-process sums over the size classes: `(allocs, alloc_bytes, frees, free_bytes)`.
    /// Per-size-class retention: (class, blocks retained, bytes retained), biggest bytes first.
    /// `class_of` is ceil-log2, so class c is roughly a 2^c-byte block. Names WHICH sizes are held
    /// rather than only how much, which is what distinguishes a leak from ordinary arena retention.
    pub fn retained_by_class() -> Vec<(usize, u64, u64)> {
        let mut v: Vec<(usize, u64, u64)> = (0..NR_CLASSES)
            .map(|c| {
                let ac = A_CNT[c].load(Relaxed);
                let fc = F_CNT[c].load(Relaxed);
                let ab = A_BYTES[c].load(Relaxed);
                let fb = F_BYTES[c].load(Relaxed);
                (c, ac.saturating_sub(fc), ab.saturating_sub(fb))
            })
            .filter(|(_, n, b)| *n > 0 && *b > 0)
            .collect();
        v.sort_by_key(|(_, _, b)| core::cmp::Reverse(*b));
        v
    }

    pub fn totals() -> (u64, u64, u64, u64) {
        let sum = |a: &[AtomicU64; NR_CLASSES]| a.iter().map(|c| c.load(Relaxed)).sum();
        (sum(&A_CNT), sum(&A_BYTES), sum(&F_CNT), sum(&F_BYTES))
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
