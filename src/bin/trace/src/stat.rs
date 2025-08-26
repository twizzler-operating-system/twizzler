use std::{
    alloc::Layout,
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use ndarray::Array1;
use ndarray_stats::QuantileExt;
use twizzler::object::ObjID;
use twizzler_abi::{
    syscall::ThreadControl,
    thread::ExecutionState,
    trace::{
        CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, ContextFaultEvent, FaultFlags, KERNEL_ALLOC,
        KernelAllocationEvent, RUNTIME_ALLOC, RuntimeAllocationEvent, SwitchFlags,
        SyscallExitEvent, THREAD_BLOCK, THREAD_CONTEXT_SWITCH, THREAD_MIGRATE, THREAD_RESUME,
        THREAD_SAMPLE, THREAD_SYSCALL_EXIT, ThreadCtxSwitch, ThreadSamplingEvent, TraceKind,
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
        .filter(|p| p.0.kind == TraceKind::Thread && p.0.event & THREAD_SYSCALL_EXIT != 0)
        .collect::<Vec<_>>();

    if syscalls.len() > 0 {
        let mut map = BTreeMap::<_, BTreeMap<u64, (Option<String>, Vec<Duration>)>>::new();

        for syscall in &syscalls {
            if let Some(data) = syscall
                .1
                .and_then(|data| data.try_cast::<SyscallExitEvent>(THREAD_SYSCALL_EXIT))
            {
                let entry = match data.data.entry.num {
                    twizzler_abi::syscall::Syscall::ThreadCtrl => map
                        .entry(data.data.entry.num)
                        .or_default()
                        .entry(data.data.entry.args[2])
                        .or_insert_with(|| {
                            (
                                ThreadControl::try_from(data.data.entry.args[2])
                                    .ok()
                                    .map(|x| format!("{:?}", x)),
                                Vec::new(),
                            )
                        }),
                    twizzler_abi::syscall::Syscall::ThreadSync => map
                        .entry(data.data.entry.num)
                        .or_default()
                        .entry(data.data.entry.args[1])
                        .or_insert_with(|| {
                            (Some(format!("len={}", data.data.entry.args[1])), Vec::new())
                        }),
                    twizzler_abi::syscall::Syscall::ObjectCtrl => map
                        .entry(data.data.entry.num)
                        .or_default()
                        .entry(data.data.entry.args[2])
                        .or_insert_with(|| {
                            (
                                match data.data.entry.args[2] {
                                    0 => Some("CreateCommit".to_string()),
                                    1 => Some("Delete".to_string()),
                                    2 => Some("Sync".to_string()),
                                    3 => Some("Preload".to_string()),
                                    _ => Some("???".to_string()),
                                },
                                Vec::new(),
                            )
                        }),
                    twizzler_abi::syscall::Syscall::MapCtrl => map
                        .entry(data.data.entry.num)
                        .or_default()
                        .entry(data.data.entry.args[2])
                        .or_insert_with(|| {
                            (
                                match data.data.entry.args[2] {
                                    0 => Some("Sync".to_string()),
                                    1 => Some("Discard".to_string()),
                                    2 => Some("Invalidate".to_string()),
                                    3 => Some("Update".to_string()),
                                    _ => Some("???".to_string()),
                                },
                                Vec::new(),
                            )
                        }),
                    _ => {
                        let entry = map
                            .entry(data.data.entry.num)
                            .or_default()
                            .entry(0)
                            .or_default();
                        entry
                    }
                };
                entry.1.push(data.data.duration.into());
            }
        }

        println!("collected {} syscalls", syscalls.len(),);

        let mut coll = map.into_iter().collect::<Vec<_>>();
        coll.sort_by_cached_key(|c| c.1.values().fold(0, |a, v| a + v.1.len()));

        let mut banner = false;
        for (k, v) in coll.iter().rev() {
            if !banner {
                banner = true;
                println!(
                    "                 SYSCALL                SUBTYPE     COUNT         MEAN       STDDEV          TOTAL"
                )
            }
            let sys = format!("{:?}", k);

            let mut coll = v.values().collect::<Vec<_>>();
            coll.sort_by_key(|c| c.1.len());
            for v in coll.iter().rev() {
                let durations = Array1::from_iter(v.1.iter().map(|d| d.as_nanos() as f64));
                let mut unit = "us";
                let mut mean = durations.mean().unwrap();
                let mut stddev = durations.std(1.);
                let total = durations.sum() / 1_000_000_000.;

                if mean <= 1000. {
                    unit = "ns";
                    mean *= 1000.;
                    stddev *= 1000.;
                } else if mean >= 1_000_000. {
                    unit = "ms";
                    mean /= 1000.;
                    stddev /= 1000.;
                }

                if durations.len() > 1 {
                    println!(
                        "    {:>20}   {:>20}   {:7}   {:8.2}{}   {:8.2}{}   {:10.2}ms",
                        sys,
                        match v.0 {
                            Some(ref st) => st.as_str(),
                            None => "",
                        },
                        durations.len(),
                        mean / 1000.,
                        unit,
                        stddev / 1000.,
                        unit,
                        total * 1000.
                    );
                } else {
                    println!(
                        "    {:>20}   {:>20}   {:7}   {:8.2}{}            -   {:10.2}ms",
                        sys,
                        match v.0 {
                            Some(ref st) => st.as_str(),
                            None => "",
                        },
                        durations.len(),
                        mean / 1000.,
                        unit,
                        total * 1000.
                    );
                }
            }
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct PerThreadData {
        migrations: usize,
        switches: usize,
        switches_to_collector: usize,
        switches_to_ktrace_kthread: usize,
    }
    let mut threads = HashMap::<ObjID, PerThreadData>::new();

    let thread_events = state.data().filter(|p| {
        p.0.kind == TraceKind::Thread
            && (p.0.event & (THREAD_CONTEXT_SWITCH | THREAD_BLOCK | THREAD_RESUME | THREAD_MIGRATE))
                != 0
    });

    for event in thread_events {
        let entry = threads.entry(event.0.thread).or_default();
        if event.0.event & THREAD_MIGRATE != 0 {
            entry.migrations += 1;
        }
        if event.0.event & THREAD_CONTEXT_SWITCH != 0 {
            entry.switches += 1;
            if let Some(data) = event
                .1
                .and_then(|d| d.try_cast::<ThreadCtxSwitch>(THREAD_CONTEXT_SWITCH))
                .map(|d| d.data)
            {
                if data.to.is_some_and(|target| target == state.collector_id) {
                    entry.switches_to_collector += 1;
                }
                if data.flags.contains(SwitchFlags::IS_TRACE) {
                    entry.switches_to_ktrace_kthread += 1;
                }
            }
        }
    }

    if !threads.is_empty() {
        println!("                            THREAD ID     MIGRATIONS     CONTEXT SWITCHES");
        for thread in &threads {
            println!(
                "     {:0>32x}        {:7}              {:7} ({:7} to tracing system)",
                thread.0.raw(),
                thread.1.migrations,
                thread.1.switches,
                thread.1.switches_to_collector + thread.1.switches_to_ktrace_kthread,
            );
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
                    println!("                            THREAD ID      COUNT");
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

    let rt_events = state.data().filter(|e| e.0.kind == TraceKind::Runtime);

    let mut rtalloc_map = HashMap::<Layout, Vec<Duration>>::new();
    let mut rtfree_map = HashMap::<Layout, Vec<Duration>>::new();
    for rte in rt_events {
        if rte.0.event & RUNTIME_ALLOC != 0 {
            if let Some(data) = rte
                .1
                .and_then(|d| d.try_cast::<RuntimeAllocationEvent>(RUNTIME_ALLOC))
                .map(|d| d.data)
            {
                let entry = if data.is_free {
                    rtfree_map.entry(data.layout).or_default()
                } else {
                    rtalloc_map.entry(data.layout).or_default()
                };
                entry.push(data.duration.into());
            }
        }
    }

    let mut coll = rtalloc_map.into_iter().collect::<Vec<_>>();
    coll.sort_by_key(|x| x.1.len());

    let mut banner = false;
    for rtalloc in coll.iter().rev() {
        if !banner {
            banner = true;
            println!("Runtime Allocation Statistics");
            println!("ALLOCATION SIZE       COUNT          MEAN        STDDEV             TOTAL")
        }
        let arr = Array1::from_iter(rtalloc.1.iter().map(|d| d.as_nanos() as f64));
        println!(
            "       {:8}    {:8}    {:8.1}ns    {:8.1}ns    {:12.4}ms",
            rtalloc.0.size(),
            arr.len(),
            arr.mean().unwrap_or(0.),
            if arr.len() == 1 { 0. } else { arr.std(1.) },
            arr.sum() / 1_000_000.
        );
    }

    let kalloc_events = state
        .data()
        .filter(|e| e.0.kind == TraceKind::Kernel && e.0.event & KERNEL_ALLOC != 0);

    let mut kalloc_map = HashMap::<Layout, Vec<Duration>>::new();
    let mut kfree_map = HashMap::<Layout, Vec<Duration>>::new();
    for kae in kalloc_events {
        if let Some(data) = kae
            .1
            .and_then(|d| d.try_cast::<KernelAllocationEvent>(KERNEL_ALLOC))
            .map(|d| d.data)
        {
            let entry = if data.is_free {
                kfree_map.entry(data.layout).or_default()
            } else {
                kalloc_map.entry(data.layout).or_default()
            };
            entry.push(data.duration.into());
        }
    }

    let mut coll = kalloc_map.into_iter().collect::<Vec<_>>();
    coll.sort_by_key(|x| x.1.len());

    let mut banner = false;
    for kalloc in coll.iter().rev() {
        if !banner {
            banner = true;
            println!("Kernel Allocation Statistics");
            println!("ALLOCATION SIZE       COUNT          MEAN        STDDEV             TOTAL")
        }
        let arr = Array1::from_iter(kalloc.1.iter().map(|d| d.as_nanos() as f64));
        println!(
            "       {:8}    {:8}    {:8.1}ns    {:8.1}ns    {:12.4}ms",
            kalloc.0.size(),
            arr.len(),
            arr.mean().unwrap_or(0.),
            if arr.len() == 1 { 0. } else { arr.std(1.) },
            arr.sum() / 1_000_000.
        );
    }
}
