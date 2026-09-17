//! Per-phase deltas of the kernel's allocation and syscall counters, printed on request from
//! userspace.
//!
//! A marker is one `Syscall::Null` with `arg0 == MAGIC`: `arg1 != 0` re-baselines silently,
//! `arg1 == 0` prints the delta since the last mark. Nothing is ever reset, so a marker misplaced
//! by a workload costs a line of output and not the run's numbers.

use core::sync::atomic::{AtomicBool, Ordering};

use twizzler_abi::syscall::Syscall;

use crate::{
    memory::{framecache, tracker::allocprofile},
    obj::pagetables::mapprobe,
    spinlock::Spinlock,
};

/// `Syscall::Null` arg0 that means "mark", next to `0x12345678`'s "dump everything and shut down".
pub const MAGIC: u64 = 0x12345679;

const NR_SYS: usize = Syscall::NumSyscalls as usize;
const NR_FC: usize = framecache::stat::NR;

struct Prev {
    alloc: [u64; allocprofile::NR],
    sys: [(usize, u64); NR_SYS],
    fc: [u64; NR_FC],
    mf: (u64, u64),
}

static PREV: Spinlock<Option<Prev>> = Spinlock::new(None);
/// Whether anything has marked. Read by the shutdown dump, which says so if nothing has.
static USED: AtomicBool = AtomicBool::new(false);

pub fn used() -> bool {
    USED.load(Ordering::Relaxed)
}

pub fn mark(rebaseline: bool) {
    USED.store(true, Ordering::Relaxed);
    let alloc = allocprofile::snapshot();
    let sys = crate::syscall::syscall_snapshot();
    let fc = framecache::stat::snapshot();
    let mf = (
        mapprobe::MF_CALLS.load(Ordering::Relaxed),
        mapprobe::MF_PAGES.load(Ordering::Relaxed),
    );

    let prev = PREV.lock().replace(Prev { alloc, sys, fc, mf });
    let Some(prev) = prev else {
        return;
    };
    if rebaseline {
        return;
    }

    // Looked up by name so appending counters cannot shift these out from under the indices.
    let idx = |n: &str| allocprofile::NAMES.iter().position(|x| *x == n).unwrap();
    let a = |n: &str| alloc[idx(n)].saturating_sub(prev.alloc[idx(n)]);
    let (idle, page, kern, reclaiming, pooled) = crate::memory::tracker::tracker_snapshot();

    logln!(
        "PERFMARK-MEM: alloc={} zeroed={} wait={} free={} | reclaim sig={} wake={} round={} | idle={} page={} kern={} pooled={} reclaiming={}",
        a("ALLOCS"),
        a("ZEROED_INLINE"),
        a("WAITS"),
        a("FREES"),
        a("RECLAIM_SIGNALS"),
        a("RECLAIM_WAKES"),
        a("RECLAIM_ROUNDS"),
        idle,
        page,
        kern,
        pooled,
        reclaiming,
    );

    // Per-call-site precharge attribution: which sites fetch frames they never use.
    let row = |tag: &str, c: u64, w: u64, u: u64| {
        logln!(
            "PERFMARK-PCSITE {}: calls={} want={} unused={} ({}% of want) want/call={}",
            tag,
            c,
            w,
            u,
            if w > 0 { u * 100 / w } else { 0 },
            if c > 0 { w * 100 / c } else { 0 },
        );
    };
    row(
        "fill ",
        a("PC_FILL_CALLS"),
        a("PC_FILL_WANT"),
        a("PC_FILL_UNUSED"),
    );
    row(
        "map  ",
        a("PC_MAP_CALLS"),
        a("PC_MAP_WANT"),
        a("PC_MAP_UNUSED"),
    );
    row(
        "other",
        a("PC_OTHER_CALLS"),
        a("PC_OTHER_WANT"),
        a("PC_OTHER_UNUSED"),
    );

    logln!(
        "PERFMARK-FILL: iters={} | allocs pool={} global={} avoid-empty={} | precharge calls={} early={} fetched={} | pool-zeroed={} pressure-declines={}",
        a("FILL_ITERS"),
        a("FA_ALLOC_POOL"),
        a("FA_ALLOC_GLOBAL"),
        a("FA_ALLOC_AVOID_EMPTY"),
        a("PRECHARGE_CALLS"),
        a("PRECHARGE_EARLY"),
        a("PRECHARGE_FETCHED"),
        a("FA_POOL_ZEROED"),
        a("FA_PARK_PRESSURE"),
    );
    logln!(
        "PERFMARK-DROP: saved={} cleared={} frames={} returned={} spills={} | PFA acquisitions: bulk={} ({} frames) single={} | pt-zero: checked={} dirty={}",
        a("FA_DROP_SAVED"),
        a("FA_DROP_CLEARED"),
        a("FA_DROP_FRAMES"),
        a("FA_TRIMMED"),
        a("FA_SPILL"),
        a("ALLOC_BULK_CALLS"),
        a("ALLOC_BULK_FRAMES"),
        a("ALLOC_SINGLE_CALLS"),
        a("PT_CHECKED"),
        a("PT_DIRTY"),
    );

    // Frame cache, printed by name from `stat::NAMES`. `frames/acq` is the amortization number:
    // it should read ~`MAG_SIZE`; ~1 means the magazines are thrashing at the boundary.
    {
        let names = framecache::stat::NAMES;
        let mut line = alloc::string::String::new();
        let mut acq = 0u64;
        let mut frames = 0u64;
        for i in 0..NR_FC {
            let d = fc[i].saturating_sub(prev.fc[i]);
            match names[i] {
                // Both directions: the depot is touched once per magazine on alloc *and* on
                // free, so an acquisition rate computed from allocs alone reads twice as good as
                // it is.
                "LOCAL_HIT" | "DEPOT_HIT" | "FREE_LOCAL" => frames += d,
                "DEPOT_ACQ" => acq = d,
                _ => {}
            }
            if d != 0 {
                line.push_str(&alloc::format!(" {}={}", names[i].to_ascii_lowercase(), d));
            }
        }
        let (clean, dirty, empty) = framecache::depths();
        logln!(
            "PERFMARK-FC:{} | frames/acq={} | depot mags: clean={} dirty={} empty={} | cached={}",
            if line.is_empty() {
                " (no activity)"
            } else {
                &line
            },
            if acq == 0 { 0 } else { frames / acq },
            clean,
            dirty,
            empty,
            framecache::cached_frames(),
        );
    }

    // `pages_per_call` is the mechanism check for fault-around batching: ~`ANON_FAULT_AROUND`
    // means the runs coalesced, ~1 means they did not and any wall-clock movement has another
    // cause.
    let mf_calls = mf.0 - prev.mf.0;
    let mf_pages = mf.1 - prev.mf.1;
    logln!(
        "PERFMARK-MAPFRAMES: calls={} pages={} pages_per_call_x100={}",
        mf_calls,
        mf_pages,
        if mf_calls > 0 {
            mf_pages * 100 / mf_calls
        } else {
            0
        },
    );

    // Syscalls, biggest time delta first. The point is attribution between phases, so a phase's
    // whole kernel bill has to be visible even when it is spread over several call numbers.
    let mut order: alloc::vec::Vec<usize> = (0..NR_SYS).collect();
    order.sort_unstable_by_key(|i| core::cmp::Reverse(sys[*i].1.saturating_sub(prev.sys[*i].1)));
    let mut sys_line = alloc::string::String::new();
    let (mut tot_c, mut tot_ns) = (0usize, 0u64);
    for i in 0..NR_SYS {
        tot_c += sys[i].0 - prev.sys[i].0;
        tot_ns += sys[i].1.saturating_sub(prev.sys[i].1);
    }
    for i in order.into_iter().take(6) {
        let c = sys[i].0 - prev.sys[i].0;
        let ns = sys[i].1.saturating_sub(prev.sys[i].1);
        if c == 0 {
            continue;
        }
        use core::fmt::Write;
        let _ = write!(sys_line, " {:?}={}/{}us", Syscall::from(i), c, ns / 1000);
    }
    logln!(
        "PERFMARK-SYS: total={}/{}us |{}",
        tot_c,
        tot_ns / 1000,
        sys_line
    );
}
