use std::collections::{BTreeMap, HashMap};

use ndarray_stats::QuantileExt;
use twizzler_abi::{
    thread::ExecutionState,
    trace::{
        CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, ContextFaultEvent, FaultFlags, SyscallEntryEvent,
        THREAD_SAMPLE, THREAD_SYSCALL_ENTRY, ThreadSamplingEvent, TraceKind,
    },
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

        let mut map = HashMap::<_, usize>::new();
        for pf in pfs {
            *map.entry(pf.data.obj).or_default() += 1;
        }

        let mut coll = map.into_iter().collect::<Vec<_>>();
        coll.sort_by_key(|c| c.1);

        let mut banner = false;
        for (k, v) in coll.iter().rev() {
            if !banner {
                banner = true;
                println!("                               OBJECT       COUNT")
            }
            println!("{:>37x}  {:10}", k.raw(), v);
        }
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

        let mut coll = map.into_iter().collect::<Vec<_>>();
        coll.sort_by_key(|c| c.1);

        let mut banner = false;
        for (k, v) in coll {
            if !banner {
                banner = true;
                println!("                 SYSCALL     COUNT")
            }
            let sys = format!("{:?}", k);
            println!("    {:>20}   {:7}", sys, v);
        }
    }

    let samples = state
        .data()
        .filter_map(|p| {
            if p.0.kind == TraceKind::Thread && p.0.event & THREAD_SAMPLE != 0 {
                Some((
                    p.0,
                    p.1.and_then(|d| d.try_cast::<ThreadSamplingEvent>(THREAD_SAMPLE))
                        .map(|d| d.data)?,
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if samples.len() > 0 {
        println!("collected {} samples", samples.len());

        let mut map = HashMap::<_, usize>::new();
        for (_head, sample) in samples {
            if sample.state == ExecutionState::Running {
                *map.entry(sample.ip).or_default() += 1usize;
            }
        }

        let mut coll = map.into_iter().collect::<Vec<_>>();
        coll.sort_by_key(|x| x.1);

        let mut banner = false;
        for (ip, count) in coll {
            if count > 1 {
                if !banner {
                    banner = true;
                    println!("PROGRAM COUNTER ADDRESS      COUNT")
                }
                println!("     {:18x}    {:7}", ip, count);
            }
        }
    }
}
