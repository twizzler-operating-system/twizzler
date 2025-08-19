use std::collections::{BTreeMap, HashMap};

use ndarray_stats::QuantileExt;
use twizzler_abi::{
    syscall::ThreadControl,
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
    println!(
        "statistics for {}, executed over {} seconds",
        state.name,
        (state.end_time - state.start_time).as_secs_f32()
    );
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
            println!("     {:0>32x}  {:10}", k.raw(), v);
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
        let mut map = BTreeMap::<_, BTreeMap<u64, (Option<String>, usize)>>::new();

        for syscall in &syscalls {
            if let Some(data) = syscall
                .1
                .and_then(|data| data.try_cast::<SyscallEntryEvent>(THREAD_SYSCALL_ENTRY))
            {
                let entry = match data.data.num {
                    twizzler_abi::syscall::Syscall::ThreadCtrl => map
                        .entry(data.data.num)
                        .or_default()
                        .entry(data.data.args[2])
                        .or_insert_with(|| {
                            (
                                ThreadControl::try_from(data.data.args[2])
                                    .ok()
                                    .map(|x| format!("{:?}", x)),
                                0,
                            )
                        }),
                    twizzler_abi::syscall::Syscall::ObjectCtrl => map
                        .entry(data.data.num)
                        .or_default()
                        .entry(data.data.args[2])
                        .or_insert_with(|| {
                            (
                                match data.data.args[2] {
                                    0 => Some("CreateCommit".to_string()),
                                    1 => Some("Delete".to_string()),
                                    2 => Some("Sync".to_string()),
                                    3 => Some("Preload".to_string()),
                                    _ => Some("???".to_string()),
                                },
                                0,
                            )
                        }),
                    twizzler_abi::syscall::Syscall::MapCtrl => map
                        .entry(data.data.num)
                        .or_default()
                        .entry(data.data.args[2])
                        .or_insert_with(|| {
                            (
                                match data.data.args[2] {
                                    0 => Some("Sync".to_string()),
                                    1 => Some("Discard".to_string()),
                                    2 => Some("Invalidate".to_string()),
                                    3 => Some("Update".to_string()),
                                    _ => Some("???".to_string()),
                                },
                                0,
                            )
                        }),
                    _ => {
                        let entry = map.entry(data.data.num).or_default().entry(0).or_default();
                        entry
                    }
                };
                *(&mut entry.1) += 1usize;
            }
        }

        println!("collected {} syscalls", syscalls.len(),);

        let mut coll = map.into_iter().collect::<Vec<_>>();
        coll.sort_by_key(|c| c.1.values().fold(0, |a, v| a + v.1));

        let mut banner = false;
        for (k, v) in coll.iter().rev() {
            if !banner {
                banner = true;
                println!("                 SYSCALL                SUBTYPE     COUNT")
            }
            let sys = format!("{:?}", k);

            let mut coll = v.values().collect::<Vec<_>>();
            coll.sort_by_key(|c| c.1);
            for v in coll.iter().rev() {
                println!(
                    "    {:>20}   {:>20}   {:7}",
                    sys,
                    match v.0 {
                        Some(ref st) => st.as_str(),
                        None => "",
                    },
                    v.1
                );
            }
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
        let mut thread_map = HashMap::<_, usize>::new();
        for (head, sample) in samples {
            if sample.state == ExecutionState::Running {
                *map.entry(sample.ip).or_default() += 1usize;
                *thread_map.entry(head.thread).or_default() += 1usize;
            }
        }
        let mut coll = thread_map.into_iter().collect::<Vec<_>>();
        coll.sort_by_key(|x| x.1);

        let mut banner = false;
        for (thread, count) in coll.iter().rev() {
            if *count > 1 {
                if !banner {
                    banner = true;
                    println!("                           THREAD ID     COUNT");
                }
                println!("     {:0>32x}    {:7}", thread.raw(), count);
            }
        }

        let mut coll = map.into_iter().collect::<Vec<_>>();
        coll.sort_by_key(|x| x.1);

        let mut banner = false;
        for (ip, count) in coll.iter().rev() {
            if *count > 1 {
                if !banner {
                    banner = true;
                    println!("PROGRAM COUNTER ADDRESS      COUNT")
                }
                println!("     {:0>18x}    {:7}", ip, count);
            }
        }
    }
}
