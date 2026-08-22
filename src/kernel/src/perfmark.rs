//! Per-phase deltas of the kernel's existing profiles, printed on request from userspace.
//!
//! The profiles ([`crate::syscall::SYSCALL_PROFILE`], the fault stages, the frame-allocation
//! counters) are cumulative and dumped once at shutdown, which answers "what did this boot cost"
//! but not "what changed between the third bench and the tenth" -- and the open findings in
//! `sysbench.md` are all of the second shape: a path that is fast fresh and slow after churn.
//!
//! A marker is one `Syscall::Null` with `arg0 == MAGIC`: `arg1 != 0` re-baselines silently,
//! `arg1 == 0` prints the delta since the last mark. Nothing is ever reset, so a marker misplaced
//! by a workload costs a line of output and not the run's numbers.

use core::sync::atomic::{AtomicBool, Ordering};

use twizzler_abi::syscall::Syscall;

use crate::{
    memory::{context::virtmem::fault, framecache, tracker::allocprofile},
    obj::pagetables::mapprobe,
    spinlock::Spinlock,
    syscall::object::{createprofile, deleteprofile},
};

/// `Syscall::Null` arg0 that means "mark", next to `0x12345678`'s "dump everything and shut down".
pub const MAGIC: u64 = 0x12345679;

const NR_SYS: usize = Syscall::NumSyscalls as usize;
const NR_STAGES: usize = fault::NR_STAGES;
const NR_CREATE: usize = createprofile::NR;
const NR_DELETE: usize = deleteprofile::NR;
const NR_MAPPROBE: usize = mapprobe::NR;
const NR_FC: usize = framecache::stat::NR;

struct Prev {
    stages: [(usize, u64); NR_STAGES],
    alloc: [u64; allocprofile::NR],
    sys: [(usize, u64); NR_SYS],
    ints: (u64, u64),
    create: [(u64, u64); NR_CREATE],
    delete: [(u64, u64); NR_DELETE],
    mapprobe: [u64; NR_MAPPROBE],
    /// Its own array rather than appended to `alloc`: `allocprofile` is indexed positionally by
    /// hand below, and every insertion into it so far has silently mislabelled every later field.
    /// A separate snapshot cannot do that to anything.
    fc: [u64; NR_FC],
}

static PREV: Spinlock<Option<Prev>> = Spinlock::new(None);
/// Whether anything has marked. Read by the shutdown dump, which says so if nothing has.
static USED: AtomicBool = AtomicBool::new(false);

pub fn used() -> bool {
    USED.load(Ordering::Relaxed)
}

pub fn mark(rebaseline: bool) {
    USED.store(true, Ordering::Relaxed);
    let stages = fault::stage_snapshot();
    let alloc = allocprofile::snapshot();
    let sys = crate::syscall::syscall_snapshot();
    let ints = crate::interrupt::snapshot();
    let create = createprofile::snapshot();
    let delete = deleteprofile::snapshot();
    let mprobe = mapprobe::snapshot();
    let fc = framecache::stat::snapshot();

    let prev = PREV.lock().replace(Prev {
        stages,
        alloc,
        sys,
        ints,
        create,
        delete,
        mapprobe: mprobe,
        fc,
    });
    let Some(prev) = prev else {
        return;
    };
    if rebaseline {
        return;
    }

    let d_stage = |i: usize| {
        (
            stages[i].0 - prev.stages[i].0,
            stages[i].1.saturating_sub(prev.stages[i].1),
        )
    };
    let a = |i: usize| alloc[i].saturating_sub(prev.alloc[i]);
    let (faults, fault_ns) = d_stage(fault::FaultStage::Total as usize);
    let (idle, page, kern, reclaiming, pooled) = crate::memory::tracker::tracker_snapshot();

    let mut stage_line = alloc::string::String::new();
    for i in 0..NR_STAGES {
        let (c, ns) = d_stage(i);
        if c == 0 {
            continue;
        }
        use core::fmt::Write;
        let _ = write!(
            stage_line,
            " {}={}/{}us",
            fault::STAGE_NAMES[i],
            c,
            ns / 1000
        );
    }

    logln!(
        "PERFMARK: faults={} {}us mean {}ns |{}",
        faults,
        fault_ns / 1000,
        if faults > 0 {
            fault_ns / faults as u64
        } else {
            0
        },
        stage_line,
    );
    logln!(
        "PERFMARK-MEM: alloc={}/{}us zeroed={}/{}us wait={}/{}us free={} | reclaim sig={} wake={} round={} | idle={} page={} kern={} pooled={} reclaiming={}",
        a(0),
        a(1) / 1000,
        a(2),
        a(3) / 1000,
        a(4),
        a(5) / 1000,
        a(6),
        a(7),
        a(8),
        a(9),
        idle,
        page,
        kern,
        pooled,
        reclaiming,
    );

    // The fill loop, parts against the whole: all four spans are timed the same cheap way, so a
    // gap between the sum and the whole is time spent between the probes rather than in any of
    // them -- which is what a preemption or an interrupt inside the loop looks like.
    let (ints, int_ns) = ints;
    logln!(
        "PERFMARK-FILL: iters={} loop={}us | empty={}us take={}us map={}us | map buckets [<1us {} <10us {} <100us {} >= {}] ints-in-map={} | all ints={}/{}us",
        a(10),
        a(11) / 1000,
        a(12) / 1000,
        a(13) / 1000,
        a(14) / 1000,
        a(15),
        a(16),
        a(17),
        a(18),
        a(19),
        ints.saturating_sub(prev.ints.0),
        int_ns.saturating_sub(prev.ints.1) / 1000,
    );

    // Named by span rather than by index, and printed only when the probe is on, because a line
    // of zeros beside a line of real numbers is how an off instrument gets read as a measurement.
    // `body` is the whole function; `gap` is what no bracket covers, which is the quantity the
    // previous split of `map_page` could only reach by subtracting two instruments.
    if mapprobe::MAP_PROBE {
        // Counts are counts; every `*_NS` slot holds raw ticks and is converted here, once.
        let m = |i: usize| mprobe[i].saturating_sub(prev.mapprobe[i]);
        let calls = m(0);
        let body = m(1);
        let spans: u64 = (2..=9).map(m).sum();
        let per = |v: u64| {
            if calls > 0 {
                mapprobe::ticks_to_ns(v / calls)
            } else {
                0
            }
        };
        logln!(
            "PERFMARK-MAPPROBE: calls={} body={}ns | cons_new={} take_fa={} precharge={} prov={} walk={} consist={} drop_fa={} drop_phys={} | gap={}ns probe={}ns populated={}",
            calls,
            per(body),
            per(m(2)),
            per(m(3)),
            per(m(4)),
            per(m(5)),
            per(m(6)),
            per(m(7)),
            per(m(8)),
            per(m(9)),
            per(body.saturating_sub(spans)),
            per(m(15)),
            m(14),
        );
        // `probe_cost` is what one `record` charges the bracket around it. `gap` should be about
        // `probe_cost` x (number of inner probes) if `map_page` is fully accounted for.
        logln!(
            "PERFMARK-MAPPROBE2: probe_interval={}ns probe_cost={}ns | gap={}ns over {} inner probes",
            per(m(15)),
            per(m(16).saturating_sub(m(15))),
            per(body.saturating_sub(spans)),
            10,
        );
        let rc = m(10);
        let rper = |v: u64| {
            if rc > 0 {
                mapprobe::ticks_to_ns(v / rc)
            } else {
                0
            }
        };
        logln!(
            "PERFMARK-RUNCONSIST: calls={} (map_page calls={}) | send={}ns reset={}ns park={}ns",
            rc,
            calls,
            rper(m(11)),
            rper(m(12)),
            rper(m(13)),
        );
        logln!("PERFMARK-RUNCONSIST2: trivial={} of {} calls", m(17), rc,);
        // What the exact-precharge predictors cost and what they bought, in one window. `entries`
        // is the breadth of the walk; `need` against `max` is the precharge removed. A `need`
        // that ever exceeds `max` is not a bug -- `cow_tables_needed` is deliberately allowed to
        // -- but it is worth noticing, so both are printed rather than only the difference.
        let tn = m(18);
        logln!(
            "PERFMARK-TABLESNEEDED: calls={} cost={}ns entries={} entries_per_call_x100={} | need={} max={} saved={}",
            tn,
            if tn > 0 {
                mapprobe::ticks_to_ns(m(19) / tn)
            } else {
                0
            },
            m(20),
            if tn > 0 { m(20) * 100 / tn } else { 0 },
            m(21),
            m(22),
            m(22) as i64 - m(21) as i64,
        );
        // Inside `walk`. `leaf_calls` is the denominator check: `Table::map` has callers other
        // than `map_page`, so these means only describe `map_page` in a window where it tracks
        // `calls`. `rest` is the loop's own overhead -- what `walk` holds that none of the three
        // brackets cover.
        let lc = m(26);
        let lper = |v: u64| {
            if lc > 0 {
                mapprobe::ticks_to_ns(v / lc)
            } else {
                0
            }
        };
        let walk_ns = per(m(6));
        let split = lper(m(23)) + lper(m(24)) + lper(m(25));
        logln!(
            "PERFMARK-WALK: leaf_calls={} (map_page calls={}) | descend={}ns leaf={}ns flush={}ns | walk={}ns rest={}ns",
            lc,
            calls,
            lper(m(23)),
            lper(m(24)),
            lper(m(25)),
            walk_ns,
            walk_ns as i64 - split as i64,
        );
        // `FrameAllocator::precharge`, split. Per *precharge* call, not per `map_page` -- the two
        // are equal on the create path and differ by 200x on the fault path, where `need == 0`
        // skips the call entirely. `global` is entered on a minority of calls, so it is printed
        // both amortized over all calls and per entry (`global_each`), because only the second
        // says what a global refill costs and only the first says what it contributes.
        let pc = m(27);
        let pcper = |v: u64| {
            if pc > 0 {
                mapprobe::ticks_to_ns(v / pc)
            } else {
                0
            }
        };
        let gc = m(33);
        logln!(
            "PERFMARK-PRECHARGE: calls={} | prov={}ns reserve={}ns fetch={}ns (pool={}ns global={}ns) | global_calls={} ({}% of calls) global_each={}ns",
            pc,
            pcper(m(28)),
            pcper(m(29)),
            pcper(m(30)),
            pcper(m(31)),
            pcper(m(32)),
            gc,
            if pc > 0 { gc * 100 / pc } else { 0 },
            if gc > 0 {
                mapprobe::ticks_to_ns(m(32) / gc)
            } else {
                0
            },
        );
        // Inside the refill, per *entry* -- these only ever run when `global` is entered, so
        // dividing them by `PC_CALLS` would understate each by ~16x on the create path.
        logln!(
            "PERFMARK-REFILL: entries={} | reclaim={}ns cas={}ns raw={}ns | global_each={}ns",
            gc,
            if gc > 0 {
                mapprobe::ticks_to_ns(m(37) / gc)
            } else {
                0
            },
            if gc > 0 {
                mapprobe::ticks_to_ns(m(38) / gc)
            } else {
                0
            },
            if gc > 0 {
                mapprobe::ticks_to_ns(m(39) / gc)
            } else {
                0
            },
            if gc > 0 {
                mapprobe::ticks_to_ns(m(32) / gc)
            } else {
                0
            },
        );
        // The two `get_frame` calls per `map_page`, against the spans that contain them.
        logln!(
            "PERFMARK-GETFRAME: populate={}ns cow_lookup={}ns leaf_lookup={}ns | descend={}ns leaf={}ns",
            lper(m(34)),
            lper(m(35)),
            lper(m(36)),
            lper(m(23)),
            lper(m(24)),
        );
    }

    // Outside the `MAP_PROBE` gate on purpose: `MF_CALLS`/`MF_PAGES` are maintained with `add`
    // rather than `tick` so they survive a probe-off build, and a counter whose only emit site is
    // behind a gate is exactly as useful as no counter. `pages_per_call` is the mechanism check
    // for fault-around batching -- ~`ANON_FAULT_AROUND` means the runs coalesced, ~1 means they
    // did not and any wall-clock movement has another cause.
    {
        let mf_calls = mapprobe::MF_CALLS.load(core::sync::atomic::Ordering::Relaxed);
        let mf_pages = mapprobe::MF_PAGES.load(core::sync::atomic::Ordering::Relaxed);
        logln!(
            "PERFMARK-MAPFRAMES: calls={} pages={} pages_per_call_x100={}",
            mf_calls,
            mf_pages,
            if mf_calls > 0 { mf_pages * 100 / mf_calls } else { 0 },
        );
    }
    logln!(
        "PERFMARK-MAP: prep={}us walk={}us consist={}us | probe floor={}us over {} probes",
        a(20) / 1000,
        a(21) / 1000,
        a(22) / 1000,
        a(23) / 1000,
        a(10),
    );
    logln!(
        "PERFMARK-DROP: map_drop={}us | fa drops: saved={}/{}us cleared={}/{}us frames={} trimmed={}",
        a(24) / 1000,
        a(25),
        a(27) / 1000,
        a(26),
        a(28) / 1000,
        a(29),
        a(30),
    );

    // The per-cpu pool's own behaviour. `take locked` is the one that should be zero and is not
    // obviously so: the pool is `#[thread_local]` (per-cpu) but its guard flag is a single global
    // atomic, so one cpu mid-take makes another fall back to a fresh empty allocator.
    logln!(
        "PERFMARK-FA: take locked={} empty={} | save locked={} | allocs pool={} global={} avoid-empty={} | precharge calls={} early={} fetched={} | parked={} park-locked={} pool-zeroed={}",
        a(31),
        a(32),
        a(33),
        a(34),
        a(35),
        a(36),
        a(37),
        a(38),
        a(39),
        a(40),
        a(41),
        a(42),
    );

    // The alloc-side drain. `unparked=` is frames the global entry points served from this cpu's
    // pool instead of the PFA; `unpark-miss=` is calls that looked and found nothing, which is
    // what separates a disabled drain from a drained-dry pool.
    logln!(
        "PERFMARK-UNPARK: unparked={} miss={} (no-pool={} empty={}) | PFA acquisitions: bulk={} single={} | pool provisioned={}",
        a(57),
        a(58),
        a(59),
        a(60),
        a(61),
        a(62),
        a(63),
    );
    logln!("PERFMARK-SPILL: frame-store spills={}", a(64));

    // Frame-allocation cost, split by path: the batch amortizes one lock acquisition over many
    // frames, the singular path does not, and the 10us attribution is precisely about that
    // difference -- so a combined figure would hide it.
    logln!(
        "PERFMARK-ALLOCT: allocs={} total | single-path {}us bulk {} frames/{}us | \
zero={}/{}us wait={}/{}us  (singular frames = allocs - bulk)",
        a(0),
        a(1) / 1000,
        a(56),
        a(55) / 1000,
        a(2),
        a(3) / 1000,
        a(4),
        a(5) / 1000,
    );

    // Why the free path declined to park, and what the save path did. A zero `parked=` with a
    // large `full=` is a saturated pool, not a disabled feature -- the two look identical in
    // `parked=` alone. `grew=` is faplan.md's Drop-allocation hazard firing.
    logln!(
        "PERFMARK-PARK: declines: not-l0={} no-tls={} pressure={} no-pool={} full={} no-cap={} \
| save: append={} grew-append={} grew-extend={} leftover={} | pt-zero: checked={} dirty={}",
        a(43),
        a(44),
        a(45),
        a(46),
        a(47),
        a(48),
        a(49),
        a(50),
        a(51),
        a(54),
        a(52),
        a(53),
    );

    // Frame cache.
    //
    // Printed **by name, from `stat::NAMES`**, not by hand-written index. The `allocprofile` lines
    // above are indexed positionally and their comments record what that has cost: three counters
    // inserted rather than appended once silently mislabelled every later field in two of them.
    // Writing this loop cost less than checking those indices would have -- and the first draft of
    // it did get them wrong, including one that indexed off the end of the array.
    if framecache::ENABLED {
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
        // `frames/acq` is **the** amortization number and the reason `DEPOT_ACQ` exists: it should
        // read ~`MAG_SIZE`. If it reads ~1 the magazines are thrashing at the boundary, and
        // nothing else on this line means what it appears to.
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

    // Stage split of `sys_object_create`, when `CREATE_PROFILE` is on. Reported per call rather
    // than as a total: the interval's create count is the divisor that makes the stages add up
    // against the `ObjectCreate` figure on the line above.
    let creates = create[createprofile::NR - 1].0 - prev.create[createprofile::NR - 1].0;
    if creates > 0 {
        let mut create_line = alloc::string::String::new();
        for i in 0..NR_CREATE {
            let c = create[i].0 - prev.create[i].0;
            if c == 0 {
                continue;
            }
            let ns = create[i].1.saturating_sub(prev.create[i].1);
            use core::fmt::Write;
            let _ = write!(create_line, " {}={}ns", createprofile::NAMES[i], ns / c);
        }
        logln!("PERFMARK-CREATE: creates={} |{}", creates, create_line);
    }

    // Same treatment for `sys_object_ctrl(Delete)`. Note `scan` contains `reap`, so the stages do
    // not sum to TOTAL -- `scan - reap` is the reapability test on an object that was not reaped.
    let deletes = delete[deleteprofile::NR - 1].0 - prev.delete[deleteprofile::NR - 1].0;
    if deletes > 0 {
        let mut delete_line = alloc::string::String::new();
        for i in 0..NR_DELETE {
            let c = delete[i].0 - prev.delete[i].0;
            if c == 0 {
                continue;
            }
            let ns = delete[i].1.saturating_sub(prev.delete[i].1);
            use core::fmt::Write;
            let _ = write!(
                delete_line,
                " {}={}ns/{}",
                deleteprofile::NAMES[i],
                ns / c,
                c
            );
        }
        logln!("PERFMARK-DELETE: deletes={} |{}", deletes, delete_line);
    }
}
