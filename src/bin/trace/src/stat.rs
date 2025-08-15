use std::collections::BTreeMap;

use ndarray_stats::QuantileExt;
use twizzler_abi::trace::{
    CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, ContextFaultEvent, FaultFlags, SyscallEntryEvent,
    THREAD_SYSCALL_ENTRY, TraceKind,
};

use crate::tracer::TracingState;

struct PfEvent {
    data: ContextFaultEvent,
}

pub fn stat(state: TracingState) {
    let data = state.data();

    let mut pfs = Vec::new();
    for entry in data.filter(|p| p.0.kind == TraceKind::Context) {
        if let Some(data) = entry
            .1
            .and_then(|data| data.try_cast::<ContextFaultEvent>(entry.0.event))
        {
            let pfe = PfEvent { data: data.data };
            pfs.push(pfe);
        }
    }

    if pfs.len() > 0 {
        let durations = pfs
            .iter()
            .map(|p| p.data.processing_time.as_nanos() as f64)
            .collect::<ndarray::Array1<_>>();

        let mean = durations.mean().unwrap();
        let _max = durations.max().unwrap();
        let _min = durations.min().unwrap();
        let stddev = durations.std(1.);
        let total = durations.sum() / 1_000_000_000.;

        println!(
            "{} pages faults, costing {}s, mean = {:5.5}us, stddev = {:5.5}us",
            pfs.len(),
            total,
            mean / 1000.,
            stddev / 1000.
        );

        let num_pager = pfs
            .iter()
            .filter(|p| p.data.flags.contains(FaultFlags::PAGER))
            .count();
        let num_large = pfs
            .iter()
            .filter(|p| p.data.flags.contains(FaultFlags::LARGE))
            .count();
        println!("{} used large pages, {} used pager", num_large, num_pager);
    }
    let tlbs = state
        .data()
        .filter(|p| {
            p.0.kind == TraceKind::Context
                && p.0.event & (CONTEXT_INVALIDATION | CONTEXT_SHOOTDOWN) != 0
        })
        .collect::<Vec<_>>();

    if tlbs.len() > 0 {
        let invalidations = tlbs
            .iter()
            .filter(|t| t.0.event & CONTEXT_INVALIDATION != 0)
            .count();
        let shootdowns = tlbs
            .iter()
            .filter(|t| t.0.event & CONTEXT_SHOOTDOWN != 0)
            .count();

        println!(
            "collected {} TLB events: {} invalidations, {} shootdowns",
            tlbs.len(),
            invalidations,
            shootdowns
        );
    }

    let syscalls = state
        .data()
        .filter(|p| p.0.kind == TraceKind::Thread && p.0.event & THREAD_SYSCALL_ENTRY != 0)
        .collect::<Vec<_>>();

    if syscalls.len() > 0 {
        let mut map = BTreeMap::<_, usize>::new();

        for syscall in &syscalls {
            if let Some(data) = syscall
                .1
                .and_then(|data| data.try_cast::<SyscallEntryEvent>(THREAD_SYSCALL_ENTRY))
            {
                *map.entry(data.data.num).or_default() += 1usize;
            }
        }

        println!("collected {} syscalls", syscalls.len(),);

        for (k, v) in map.iter() {
            let sys = format!("{:?}", k);
            println!("    {:>20}: {:5}", sys, v);
        }
    }
}
