//! Counter sampling: one `Sample` is every leak-relevant number the system will tell us, flattened
//! into a fixed vector so the fitting code can stay generic over counters.

use twizzler_abi::syscall::{sys_memory_stats, sys_object_stats, sys_sctx_stats, sys_thread_stats};

/// How a counter behaves when nothing is wrong, which decides how to read a positive slope.
///
/// A `Cumulative` counter rises forever by construction -- a slope on it is a rate, never a leak --
/// and reporting those alongside the others would bury every real finding in noise.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A population that should return to its starting value. A sustained slope here is a leak.
    Level,
    /// A monotone since-boot total. Slope is a rate; informative, never a verdict.
    Cumulative,
}

pub struct CounterDef {
    pub name: &'static str,
    pub kind: Kind,
}

macro_rules! counters {
    ($($name:literal : $kind:ident),* $(,)?) => {
        pub const COUNTERS: &[CounterDef] = &[
            $(CounterDef { name: $name, kind: Kind::$kind }),*
        ];
        pub const NR_COUNTERS: usize = [$($name),*].len();
    };
}

counters! {
    // Frame tracker. The four that matter most: a leak lands in kernel_used or page_data, and
    // idle is what it comes out of.
    "trk.idle": Level,
    "trk.kernel_used": Level,
    "trk.page_data": Level,
    "trk.pager_outstanding": Level,
    "trk.waiting": Level,
    "trk.reclaiming": Level,
    "trk.allocated": Cumulative,
    "trk.freed": Cumulative,
    "trk.reclaimed": Cumulative,
    // Frames held in per-cpu frame caches. Not a leak signal on its own -- it is the *correction*
    // for one. A cached frame is still charged to `kernel_used`/`page_data` and is still off the
    // allocator's free list, so it depresses `trk.idle` and `mem.free_pages` and inflates its
    // class counter exactly as a leaked frame does. `trk.freed` reading slope 0.0000 in 31 of 42
    // ops -- and identically for `p1-leak-object`, the deliberate control -- is that confusion
    // measured (framecache.md 1.6).
    "trk.pooled": Level,
    // `kernel_used + page_data - pooled`: the live charged population, cache occupancy removed.
    // This is the counter to read for a frame leak; the two class counters are kept above because
    // *which* class moved is still the routing information, and because a divergence between this
    // and them is itself a finding (the cache mis-charging on hand-out).
    "trk.charged_net": Level,
    // Physical allocator's own view, which excludes frames parked in precharge pools.
    "mem.free_pages": Level,
    "mem.kalloc_bytes": Level,
    "mem.page_faults": Cumulative,
    "mem.tlb_shootdowns": Cumulative,
    // Objects. nr_objects against nr_pending_delete is the routing decision: pending flat means
    // nobody asked for deletion (userspace), both rising means reaping is stuck (kernel).
    "obj.objects": Level,
    "obj.mapped": Level,
    "obj.pending_delete": Level,
    "obj.handles": Level,
    "obj.ties": Level,
    // Threads. nr_pending_exit is the one to watch: cleanup_exited pops a single thread per call.
    "thr.threads": Level,
    "thr.blocked": Level,
    "thr.pending_exit": Level,
    // The population no global counter could see: `exit` removes a thread from ALL_THREADS before
    // pushing it to a per-cpu cleanup list, so a thread awaiting reap is in neither `thr.threads`
    // nor `thr.pending_exit`. Each one holds a 2 MiB kernel stack.
    "thr.exited_backlog": Level,
    "thr.reaped": Cumulative,
    "sctx.sctx": Level,
    "sctx.cached": Level,
    // Monitor. space.mapped minus space.active is the deferred-unmap population.
    "mon.space_mapped": Level,
    "mon.space_active": Level,
    "mon.threads": Level,
    "mon.compartments": Level,
    "mon.comp_handles": Level,
    "mon.lib_handles": Level,
    "mon.libs": Level,
    // This compartment's own address-space slots: finite and not recycled.
    "self.slots": Level,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Sample {
    pub v: [u64; NR_COUNTERS],
}

impl Sample {
    /// Equality over `Level` counters only, which is what "the system has stopped moving" means.
    ///
    /// Full equality is the wrong test and never fires: `trk.allocated`, `mem.page_faults` and the
    /// rest of the cumulative set rise forever from background activity, so two consecutive
    /// samples are never identical and every quiesce would report non-convergence.
    pub fn settled_eq(&self, other: &Self) -> bool {
        COUNTERS
            .iter()
            .enumerate()
            .filter(|(_, c)| c.kind == Kind::Level)
            .all(|(i, _)| self.v[i] == other.v[i])
    }
}

impl Sample {
    pub fn take() -> Self {
        let mem = sys_memory_stats();
        let t = &mem.tracker;
        let obj = sys_object_stats();
        let thr = sys_thread_stats();
        let sctx = sys_sctx_stats();
        let mon = monitor_api::stats();

        // A monitor gate call that fails must not be read as "the counter went to zero" -- that
        // would manufacture a step change in every series at once. Carry it as absent instead.
        let (sm, sa, mt, mc, mch, mlh, ml) = match mon {
            Some(m) => (
                m.space.mapped as u64,
                m.space.active as u64,
                m.thread_mgr.nr_threads as u64,
                m.comp_mgr.nr_compartments as u64,
                m.handles.nr_comp_handles as u64,
                m.handles.nr_lib_handles as u64,
                m.dynlink.nr_libs as u64,
            ),
            None => (
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
                u64::MAX,
            ),
        };

        Self {
            v: [
                t.idle as u64,
                t.kernel_used as u64,
                t.page_data as u64,
                t.pager_outstanding as u64,
                t.waiting as u64,
                t.reclaiming as u64,
                t.allocated as u64,
                t.freed as u64,
                t.reclaimed as u64,
                t.pooled as u64,
                // `saturating_sub`: the three reads are not mutually consistent (`fill_stats` says
                // so), so a cache that grew between the class reads and the gauge read can make
                // this momentarily negative. Clamping is right for a `Level` series -- a wrapped
                // u64 would be a fake step change of 2^64 and would flag every op.
                (t.kernel_used as u64 + t.page_data as u64).saturating_sub(t.pooled as u64),
                mem.free_bytes() as u64 / 4096,
                mem.kalloc_bytes() as u64,
                mem.page_fault_count as u64,
                mem.tlb_shootdown_count as u64,
                obj.nr_objects as u64,
                obj.nr_mapped as u64,
                obj.nr_pending_delete as u64,
                obj.nr_handles as u64,
                obj.nr_ties as u64,
                thr.nr_threads as u64,
                thr.nr_blocked as u64,
                thr.nr_pending_exit as u64,
                thr.nr_exited_backlog as u64,
                thr.nr_reaped as u64,
                sctx.nr_sctx as u64,
                sctx.nr_cached as u64,
                sm,
                sa,
                mt,
                mc,
                mch,
                mlh,
                ml,
                count_slots(),
            ],
        }
    }
}

/// Number of slots with an object mapped into this compartment's address space.
fn count_slots() -> u64 {
    let mut buf = [0u64; 256];
    let mut total = 0u64;
    let mut off = 0usize;
    loop {
        match twizzler_abi::syscall::sys_enumerate_slots(&mut buf, off) {
            Ok(n) => {
                total += n as u64;
                if n < buf.len() {
                    return total;
                }
                off += n;
            }
            // Absent, not zero: see the monitor note above.
            Err(_) => return u64::MAX,
        }
    }
}
