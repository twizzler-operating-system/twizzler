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
    memory::{context::virtmem::fault, tracker::allocprofile},
    spinlock::Spinlock,
    syscall::object::createprofile,
};

/// `Syscall::Null` arg0 that means "mark", next to `0x12345678`'s "dump everything and shut down".
pub const MAGIC: u64 = 0x12345679;

const NR_SYS: usize = Syscall::NumSyscalls as usize;
const NR_STAGES: usize = fault::NR_STAGES;
const NR_CREATE: usize = createprofile::NR;

struct Prev {
    stages: [(usize, u64); NR_STAGES],
    alloc: [u64; allocprofile::NR],
    sys: [(usize, u64); NR_SYS],
    ints: (u64, u64),
    create: [(u64, u64); NR_CREATE],
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

    let prev = PREV.lock().replace(Prev {
        stages,
        alloc,
        sys,
        ints,
        create,
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
    let (idle, page, kern, reclaiming) = crate::memory::tracker::tracker_snapshot();

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
        "PERFMARK-MEM: alloc={}/{}us zeroed={}/{}us wait={}/{}us free={} | reclaim sig={} wake={} round={} | idle={} page={} kern={} reclaiming={}",
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
        "PERFMARK-FA: take locked={} empty={} | save locked={} | allocs pool={} global={} avoid-empty={} | precharge calls={} early={} fetched={}",
        a(31),
        a(32),
        a(33),
        a(34),
        a(35),
        a(36),
        a(37),
        a(38),
        a(39),
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
}
