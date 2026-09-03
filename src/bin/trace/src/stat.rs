use std::{
    alloc::Layout,
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use ndarray::Array1;
use ndarray_stats::QuantileExt;
use twizzler::object::ObjID;
use twizzler_abi::{
    object::Protections,
    syscall::{MapFlags as SysMapFlags, MapInfo, ThreadControl, sys_object_read_map},
    thread::ExecutionState,
    trace::{
        CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, ContextFaultEvent, FaultFlags, KERNEL_ALLOC,
        KernelAllocationEvent, RUNTIME_ALLOC, RuntimeAllocationEvent, SwitchFlags,
        SyscallExitEvent, THREAD_BLOCK, THREAD_CONTEXT_SWITCH, THREAD_MIGRATE, THREAD_RESUME,
        THREAD_SAMPLE, THREAD_SYSCALL_EXIT, ThreadCtxSwitch, ThreadSamplingEvent, TraceKind,
    },
};

use crate::tracer::{LoadedLib, TracingState};

struct PfEvent {
    data: ContextFaultEvent,
}

/// Which library a pc lands in, and the offset within it.
///
/// Compartment-scoped by construction: slots are per-context, so the same pc names different code
/// in two compartments.
fn resolve<'a>(libs: &'a [LoadedLib], pc: u64) -> Option<(&'a str, u64)> {
    libs.iter()
        .find(|l| pc >= l.start as u64 && pc < (l.start + l.len) as u64)
        // Offset from the *slot base*, not from `l.start`. A library's load address excludes the
        // null page (`start == slot_base + NULLPAGE_SIZE`) while its first PT_LOAD vaddr is
        // `0x1000`, so subtracting `start` would report every symbol 0x1000 low.
        .map(|l| (l.name.as_str(), pc & 0x3fff_ffff))
}

fn by_pc_has(pcs: &[((ObjID, u64), usize)], sctx: ObjID) -> bool {
    pcs.iter().any(|((s, _), _)| *s == sctx)
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

    #[derive(Debug, Clone, Default)]
    struct PerThreadData {
        migrations: usize,
        switches: usize,
        switches_to_collector: usize,
        switches_to_ktrace_kthread: usize,
        cpu_map: HashMap<u64, usize>,
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
            *entry.cpu_map.entry(event.0.cpuid).or_default() += 1;
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
        println!("                                                         ON CPUs");
        for thread in &threads {
            println!(
                "     {:0>32x}        {:7}              {:7} ({:7} to tracing system)",
                thread.0.raw(),
                thread.1.migrations,
                thread.1.switches,
                thread.1.switches_to_collector + thread.1.switches_to_ktrace_kthread,
            );

            let mut cpumap = thread.1.cpu_map.iter().collect::<Vec<_>>();
            cpumap.sort_by_key(|x| *x.0);
            print!("                                                         [",);
            for (i, cpu) in cpumap.iter().enumerate() {
                if i != 0 {
                    print!(", ")
                }
                print!("{}:{}", cpu.0, cpu.1);
            }
            println!("]");
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

        // Attribute by compartment first. In a build almost every sample lands in a child
        // compartment, so a flat pc histogram would mix code from contexts that do not share a
        // slot layout, and read as noise.
        let mut by_comp = HashMap::<ObjID, usize>::new();
        let mut by_pc = HashMap::<(ObjID, u64), usize>::new();
        let mut by_thread = HashMap::<ObjID, (ObjID, usize)>::new();
        // The memory context is what can translate an address to the object actually mapped at
        // that slot; the compartment's reported library list cannot (see the load-map note).
        let mut mctx_of = HashMap::<(ObjID, u64), ObjID>::new();
        // (ip, di, cx): the two scratch registers the kernel captures without touching memory.
        let mut samples_reg: Vec<(u64, u64, u64)> = Vec::new();
        let mut running = 0usize;
        for (head, sample) in samples {
            if sample.state != ExecutionState::Running {
                continue;
            }
            running += 1;
            *by_comp.entry(head.sctx).or_default() += 1usize;
            *by_pc.entry((head.sctx, sample.ip)).or_default() += 1usize;
            mctx_of.insert((head.sctx, sample.ip), head.mctx);
            by_thread.entry(head.thread).or_insert((head.sctx, 0)).1 += 1;
            samples_reg.push((sample.ip, sample.di, sample.cx));
        }
        let name_of = |sctx: ObjID| -> String {
            state
                .comp_maps
                .iter()
                .find(|c| c.sctx == sctx)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| {
                    if sctx.raw() == 0 {
                        "<kernel/no sctx>".to_string()
                    } else {
                        format!("<{:x}>", sctx.raw())
                    }
                })
        };
        let empty: Vec<LoadedLib> = Vec::new();
        let libs_of = |sctx: ObjID| -> &[LoadedLib] {
            state
                .comp_maps
                .iter()
                .find(|c| c.sctx == sctx)
                .map(|c| c.libs.as_slice())
                .unwrap_or(&empty)
        };

        let mut comps = by_comp.into_iter().collect::<Vec<_>>();
        comps.sort_by_key(|x| x.1);
        println!(
            "\n{:>8}  {:>7}   COMPARTMENT ({} running samples)",
            "COUNT", "%", running
        );
        for (sctx, count) in comps.iter().rev() {
            println!(
                "{:>8}  {:>6.2}%   {}",
                count,
                100.0 * *count as f64 / running as f64,
                name_of(*sctx)
            );
        }

        let mut threads = by_thread.into_iter().collect::<Vec<_>>();
        threads.sort_by_key(|x| x.1.1);
        println!("\n{:>8}  {:>7}   THREAD / COMPARTMENT", "COUNT", "%");
        for (thread, (sctx, count)) in threads.iter().rev().take(24) {
            println!(
                "{:>8}  {:>6.2}%   {:0>32x}  {}",
                count,
                100.0 * *count as f64 / running as f64,
                thread.raw(),
                name_of(*sctx)
            );
        }

        // Percentages are of *running* samples, and the count<=1 tail is reported rather than
        // silently dropped. Suppressing singletons without saying so makes whatever survives look
        // dominant -- which is exactly how a 2.1% pc got read as a 27.4% hot spot.
        let mut pcs = by_pc.into_iter().collect::<Vec<_>>();
        pcs.sort_by_key(|x| x.1);
        let (mut shown, mut shown_pcs) = (0usize, 0usize);
        println!(
            "\n{:>8}  {:>7}   PC = LIBRARY + OFFSET (symbolize the offset, not the pc)",
            "COUNT", "%"
        );
        for ((sctx, ip), count) in pcs.iter().rev() {
            if *count <= 1 {
                continue;
            }
            shown += *count;
            shown_pcs += 1;
            if shown_pcs > 40 {
                continue;
            }
            match resolve(libs_of(*sctx), *ip) {
                Some((lib, off)) => println!(
                    "{:>8}  {:>6.2}%   {}+{:x}   [{}]",
                    count,
                    100.0 * *count as f64 / running as f64,
                    lib,
                    off,
                    name_of(*sctx)
                ),
                None => {
                    let slot = (*ip >> 30) as usize;
                    let off = *ip & 0x3fff_ffff;
                    let mctx = mctx_of.get(&(*sctx, *ip)).copied().unwrap_or(0.into());
                    // One vm context is shared across security contexts, so the slot table is
                    // global and the tracer's own context resolves any address. The per-context
                    // handle is only a fallback in case that ever stops being true.
                    // Prefer the live-resolved map: by report time every rustc has exited and
                    // its slots no longer resolve, which is what left a whole build unsymbolized.
                    let via = state
                        .slot_map
                        .get(&slot)
                        .copied()
                        .map(|id| MapInfo {
                            id,
                            prot: Protections::empty(),
                            slot,
                            flags: SysMapFlags::empty(),
                        })
                        .ok_or(())
                        .or_else(|_| sys_object_read_map(None, slot).map_err(|_| ()))
                        .or_else(|_| sys_object_read_map(Some(mctx), slot).map_err(|_| ()));
                    match via {
                        Ok(mi) => {
                            let named = state
                                .comp_maps
                                .iter()
                                .flat_map(|c| c.libs.iter())
                                .find(|l| l.objid == mi.id)
                                .map(|l| l.name.clone())
                                .unwrap_or_else(|| format!("obj:{:x}", mi.id.raw()));
                            println!(
                                "{:>8}  {:>6.2}%   {}+{:x}   [{}]",
                                count,
                                100.0 * *count as f64 / running as f64,
                                named,
                                off,
                                name_of(*sctx)
                            )
                        }
                        Err(_) => println!(
                            "{:>8}  {:>6.2}%   slot {} +{:x} (unresolved{})   [{}]",
                            count,
                            100.0 * *count as f64 / running as f64,
                            slot,
                            off,
                            "",
                            name_of(*sctx)
                        ),
                    }
                }
            }
        }
        let tail = running - shown;
        println!(
            "{:>8}  {:>6.2}%   SUPPRESSED TAIL, in {} single-sample pcs; {} pcs total, {} listed",
            tail,
            100.0 * tail as f64 / running as f64,
            pcs.len() - shown_pcs,
            pcs.len(),
            shown_pcs.min(40)
        );

        // What the hottest leaf is writing. A sample cannot carry a return address -- reading
        // `[sp]` from the tick path halted the processor, and by report time the stack word is
        // overwritten -- but the *registers* cost nothing to capture. For a `rep stos` leaf that
        // is enough to name the destination object and bound the size of each individual call,
        // which between them identify the caller far more sharply than a flat pc histogram does.
        // Not `pcs.last()`: the highest-count pc is the idle thread's pc=0.
        if let Some(((_, hot_ip), _)) = pcs
            .iter()
            .rev()
            .find(|((_, ip), _)| *ip != 0 && ip & 0x3fff_ffff != 0)
        {
            let hot_slot_off = hot_ip & 0x3fff_ffff;
            let mut dests = HashMap::<usize, usize>::new();
            let mut sizes = HashMap::<u32, usize>::new();
            let mut bytes = 0u128;
            let mut tried = 0usize;
            for (ip, di, cx) in &samples_reg {
                if ip & 0x3fff_ffff != hot_slot_off {
                    continue;
                }
                tried += 1;
                *dests.entry((*di >> 30) as usize).or_default() += 1usize;
                // `cx` counts what is *left* to store, so `cx * 8` is a lower bound on the size
                // of this particular memset -- uniform sampling within a rep makes the observed
                // maximum approach the real size.
                let rem = cx.saturating_mul(8);
                bytes += rem as u128;
                *sizes.entry(64 - (rem | 1).leading_zeros()).or_default() += 1usize;
            }
            let name_slot = |slot: usize| -> String {
                state
                    .slot_map
                    .get(&slot)
                    .map(|id| {
                        state
                            .comp_maps
                            .iter()
                            .flat_map(|c| c.libs.iter())
                            .find(|l| l.objid == *id)
                            .map(|l| l.name.clone())
                            .unwrap_or_else(|| format!("obj:{:x}", id.raw()))
                    })
                    .unwrap_or_else(|| "<unmapped by report time>".to_string())
            };
            println!(
                "\nHOTTEST LEAF (+{:x}): {} samples; mean remaining {} bytes",
                hot_slot_off,
                tried,
                if tried == 0 {
                    0
                } else {
                    (bytes / tried as u128) as u64
                }
            );
            let mut dd = dests.into_iter().collect::<Vec<_>>();
            dd.sort_by_key(|x| x.1);
            println!("{:>8}  {:>7}   DESTINATION (slot of di)", "COUNT", "%");
            for (slot, count) in dd.iter().rev().take(12) {
                println!(
                    "{:>8}  {:>6.2}%   slot {:<5} {}",
                    count,
                    100.0 * *count as f64 / tried.max(1) as f64,
                    slot,
                    name_slot(*slot)
                );
            }
            let mut ss = sizes.into_iter().collect::<Vec<_>>();
            ss.sort_by_key(|x| x.0);
            println!(
                "{:>8}  {:>7}   REMAINING BYTES (cx*8, log2 bucket)",
                "COUNT", "%"
            );
            for (lg, count) in ss.iter() {
                println!(
                    "{:>8}  {:>6.2}%   <= {}",
                    count,
                    100.0 * *count as f64 / tried.max(1) as f64,
                    1u64 << lg
                );
            }
        }

        // Without this a histogram cannot be symbolized safely: it names the exact object each
        // library was loaded from, so a rebuilt copy on the host is detectable rather than
        // silently producing whatever now sits at that offset.
        for c in &state.comp_maps {
            if !by_pc_has(&pcs, c.sctx) {
                continue;
            }
            println!(
                "\nLOAD MAP for {} (symbolize against these objects only)",
                c.name
            );
            println!("{:>18}  {:>18}  {:>34}  NAME", "START", "LEN", "OBJID");
            for l in &c.libs {
                println!(
                    "{:>18x}  {:>18x}  {:>34x}  {}",
                    l.start,
                    l.len,
                    l.objid.raw(),
                    l.name
                );
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
