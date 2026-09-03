use std::{
    collections::{BTreeMap, HashSet},
    fmt::Debug,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::Builder,
    time::Instant,
    usize,
};

use miette::IntoDiagnostic;
use monitor_api::{CompartmentFlags, CompartmentHandle};
use twizzler::{
    BaseType, Invariant,
    object::{MapFlags, ObjID, Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{
    syscall::{
        EnumerateKind, ObjectCreate, PERTHREAD_TRACE_GEN_SAMPLE, ThreadSchedStats, ThreadSctxIds,
        ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
        ThreadSyncWake, TraceSpec, sys_enumerate, sys_ktrace, sys_object_read_map,
        sys_thread_change_state, sys_thread_read_sctx_ids, sys_thread_read_stats,
        sys_thread_self_id, sys_thread_set_trace_events, sys_thread_sync,
    },
    thread::ExecutionState,
    trace::{
        THREAD_SAMPLE, ThreadSamplingEvent, TraceBase, TraceData, TraceEntryFlags, TraceEntryHead,
        TraceKind,
    },
};

use crate::Cli;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Setup,
    Ready,
    Running,
    Done,
}

pub struct TraceSource {
    objects: Vec<Object<BaseWrap>>,
    end_point: u64,
    pub total: u64,
}

pub struct TracingState {
    pub kernel_source: TraceSource,
    pub user_source: Option<TraceSource>,
    state: State,
    pub start_time: Instant,
    pub end_time: Instant,
    pub name: String,
    pub nr_wakes: usize,
    pub collector_id: ObjID,
    /// Where each library was loaded, captured while the target is alive. A PC histogram is
    /// meaningless without this: symbolizing against a copy of a library that was rebuilt after
    /// the run silently yields whatever now sits at that offset. `objid` pins the exact object the
    /// library was loaded from, which a path does not.
    pub load_map: Vec<LoadedLib>,
    /// Load maps for every compartment a sample was seen from, keyed by security context.
    pub comp_maps: Vec<CompMap>,
    /// Cumulative per-thread cpu accounting, sampled at 1 Hz while the run is live. Independent
    /// of the profile's own samples: see the note in [`arm`] on why sample share is not cpu time.
    pub thread_stats: BTreeMap<ObjID, ThreadSchedStats>,
    /// slot -> object mapped there, resolved *while the run is live*.
    ///
    /// The slot table is global (one vm context, many security contexts), so the tracer's own
    /// context resolves any address -- but only while the object is still mapped. Every rustc a
    /// build spawns has exited by report time, which is why resolving late yields nothing.
    pub slot_map: BTreeMap<usize, ObjID>,
}

pub struct LoadedLib {
    pub name: String,
    pub start: usize,
    pub len: usize,
    pub objid: ObjID,
}

/// One compartment's identity and load map, snapshotted while it is alive.
///
/// Needed per-compartment, not once: slots are per-context, so the same pc means different code
/// in two compartments, and a child (each rustc invocation) is gone by the time the report runs.
pub struct CompMap {
    pub sctx: ObjID,
    pub name: String,
    pub libs: Vec<LoadedLib>,
    /// Held for the life of the trace. A compartment is torn down only when its use count hits
    /// zero *and* it has exited (`CompartmentMgr::dec_use_count`), so keeping the handle pins it
    /// and its object mappings. Without this every rustc a build spawns is reaped before the
    /// report runs, unmapping the slots its samples point at.
    pub _keepalive: CompartmentHandle,
}

impl Debug for TraceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TraceSource {{ {} objects, end_point: {}, total: {} }}",
            self.objects.len(),
            self.end_point,
            self.total,
        )
    }
}

impl Debug for TracingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TracingState {{ kernel_source: {:?}, user_source: {:?}, state: {:?}, start_time: {:?}, end_time: {:?}, name: {:?} }}",
            self.kernel_source,
            self.user_source,
            self.state,
            self.start_time,
            self.end_time,
            self.name,
        )
    }
}

#[derive(BaseType, Invariant)]
#[repr(transparent)]
pub struct BaseWrap(pub TraceBase);

impl TracingState {
    fn new(
        name: String,
        specs: &[TraceSpec],
        user_prime: Option<Object<BaseWrap>>,
    ) -> miette::Result<Self> {
        let prime = ObjectBuilder::new(ObjectCreate::default())
            .build(BaseWrap(TraceBase {
                start: 0,
                end: AtomicU64::new(0),
            }))
            .into_diagnostic()?;

        for spec in specs {
            sys_ktrace(prime.id(), Some(spec)).into_diagnostic()?;
        }

        let user_source = user_prime.map(|up| TraceSource {
            objects: vec![up],
            end_point: 0,
            total: 0,
        });

        let kernel_source = TraceSource {
            objects: vec![prime],
            end_point: 0,
            total: 0,
        };

        Ok(Self {
            kernel_source,
            user_source,
            state: State::Setup,
            start_time: Instant::now(),
            end_time: Instant::now(),
            name,
            nr_wakes: 0,
            collector_id: 0.into(),
            load_map: Vec::new(),
            comp_maps: Vec::new(),
            thread_stats: BTreeMap::new(),
            slot_map: BTreeMap::new(),
        })
    }

    fn collect(&mut self) -> miette::Result<[Option<ThreadSyncSleep>; 2]> {
        let s1 = self.kernel_source.collect()?;
        let s2 = self.user_source.as_mut().and_then(|us| us.collect().ok());
        Ok([Some(s1), s2])
    }

    pub fn data(&self) -> impl Iterator<Item = (&'_ TraceEntryHead, Option<&'_ TraceData<()>>)> {
        self.kernel_source.data().chain(
            self.user_source
                .as_ref()
                .map(|us| us.data())
                .unwrap_or(TraceDataIter::empty()),
        )
    }
}

impl TraceSource {
    fn collect(&mut self) -> miette::Result<ThreadSyncSleep> {
        let mut current = self.objects.last().unwrap();
        let posted_end = current.base().0.end.load(Ordering::SeqCst);
        let start_point = self.end_point.max(current.base().0.start);
        tracing::trace!(
            "collect {:x}: {:x} {:x}: {}",
            self.end_point,
            posted_end,
            start_point,
            self.objects.len()
        );
        if self.end_point != posted_end {
            let amount = posted_end.saturating_sub(start_point);

            if amount > 0 {
                self.total += amount;

                // scan for next object directives
                let mut offset = 0usize;
                while offset < amount as usize {
                    let header = current
                        .lea(start_point as usize + offset, size_of::<TraceEntryHead>())
                        .unwrap()
                        .cast::<TraceEntryHead>();
                    let header = unsafe { &*header };
                    if header.flags.contains(TraceEntryFlags::NEXT_OBJECT) {
                        tracing::debug!("got next tracing object: {}", header.extra_or_next);
                        let next = unsafe {
                            Object::<BaseWrap>::map_unchecked(header.extra_or_next, MapFlags::READ)
                                .into_diagnostic()
                        }?;
                        self.objects.push(next);
                        current = self.objects.last().unwrap();
                        self.end_point = current.base().0.start;
                        return self.collect();
                    } else {
                        offset += size_of::<TraceEntryHead>();
                        if header.flags.contains(TraceEntryFlags::HAS_DATA) {
                            let data_header = current
                                .lea(start_point as usize + offset, size_of::<TraceData<()>>())
                                .unwrap()
                                .cast::<TraceData<()>>();
                            offset += (unsafe { *data_header }).len as usize;
                        }
                    }
                }
                if offset == amount as usize {
                    self.end_point += amount;
                }
            }
        }

        Ok(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&current.base().0.end),
            start_point,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ))
    }

    pub fn data(&self) -> TraceDataIter<'_> {
        TraceDataIter {
            state: Some(self),
            pos: 0,
            inner_pos: 0,
        }
    }
}

pub struct TraceDataIter<'a> {
    state: Option<&'a TraceSource>,
    pos: usize,
    inner_pos: u64,
}

impl TraceDataIter<'_> {
    pub fn empty() -> Self {
        Self {
            state: None,
            pos: 0,
            inner_pos: 0,
        }
    }
}

#[allow(dead_code)]
struct Tracer {
    state: Mutex<TracingState>,
    specs: Vec<TraceSpec>,
    state_cv: Condvar,
    notifier: AtomicU64,
}

impl<'a> Iterator for TraceDataIter<'a> {
    type Item = (&'a TraceEntryHead, Option<&'a TraceData<()>>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.state.is_none() {
            return None;
        }
        let obj = self.state.as_ref().unwrap().objects.get(self.pos)?;
        let start_pos = self.inner_pos.max(obj.base().0.start);
        self.inner_pos = start_pos;
        let end = obj.base().0.end.load(Ordering::SeqCst);
        if start_pos + size_of::<TraceEntryHead>() as u64 > end {
            self.pos += 1;
            self.inner_pos = 0;
            return self.next();
        }
        let mut len = size_of::<TraceEntryHead>();
        let header = obj
            .lea(start_pos as usize, len)
            .unwrap()
            .cast::<TraceEntryHead>();
        let header = unsafe { header.as_ref().unwrap() };
        let data = if header.flags.contains(TraceEntryFlags::HAS_DATA) {
            let data_header = obj
                .lea(
                    start_pos as usize + size_of::<TraceEntryHead>(),
                    size_of::<TraceData<()>>(),
                )
                .unwrap()
                .cast::<TraceData<()>>();
            let data_header = unsafe { data_header.as_ref().unwrap() };
            let data = obj
                .lea(
                    start_pos as usize + size_of::<TraceEntryHead>(),
                    data_header.len as usize,
                )
                .unwrap()
                .cast::<TraceData<()>>();
            let data = unsafe { data.as_ref().unwrap() };
            len += data.len as usize;
            Some(data)
        } else {
            None
        };

        self.inner_pos += len as u64;

        Some((header, data))
    }
}

impl Tracer {
    fn set_state(&self, new_state: State) {
        tracing::trace!("setting tracing state: {:?}", new_state);
        let mut guard = self.state.lock().unwrap();
        guard.state = new_state;
        self.state_cv.notify_all();
    }

    fn wait_for(&self, target_state: State) {
        tracing::trace!("wait for tracing state: {:?}", target_state);
        let mut guard = self.state.lock().unwrap();
        while guard.state != target_state {
            guard = self.state_cv.wait(guard).unwrap();
        }
    }

    fn notify_exit(&self) {
        let wake = ThreadSyncWake::new(ThreadSyncReference::Virtual(&self.notifier), usize::MAX);
        self.notifier.store(1, Ordering::SeqCst);
        let _ = sys_thread_sync(&mut [ThreadSync::new_wake(wake)], None).inspect_err(|e| {
            tracing::warn!("failed to notify exit: {}", e);
        });
    }
}

fn collector(tracer: &Tracer) {
    tracer.state.lock().unwrap().collector_id = sys_thread_self_id();
    tracer.set_state(State::Ready);
    let mut nr_wakes = 0;
    loop {
        let mut guard = tracer.state.lock().unwrap();
        let Ok(waiter) = guard.collect().inspect_err(|e| {
            tracing::error!("failed to collect trace data: {}", e);
        }) else {
            continue;
        };

        if tracer.notifier.load(Ordering::SeqCst) == 0 {
            drop(guard);
            let mut waiters = [
                ThreadSync::new_sleep(waiter[0].unwrap()),
                ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&tracer.notifier),
                    0,
                    ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                )),
                ThreadSync::new_sleep(waiter[1].unwrap_or(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(core::ptr::null()),
                    0,
                    ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                ))),
            ];
            let mut waiters = waiters.as_mut_slice();
            tracing::trace!(
                "collector is waiting for data: {} {} {}",
                waiters[0].ready(),
                waiters[1].ready(),
                if waiter[1].is_some() {
                    if waiters[2].ready() { "true" } else { "false" }
                } else {
                    "-"
                }
            );
            if waiter[1].is_none() {
                waiters = &mut waiters[0..2];
            }
            if waiters.iter().all(|w| !w.ready()) {
                let _ = sys_thread_sync(waiters, None).inspect_err(|e| {
                    tracing::warn!("failed to thread sync: {}", e);
                });
                nr_wakes += 1;
            }
        } else {
            tracing::trace!("collector was notified of exit");

            let _ = sys_ktrace(guard.kernel_source.objects.first().unwrap().id(), None)
                .inspect_err(|e| {
                    tracing::error!("failed to disable tracing: {}", e);
                });
            let _ = guard.collect().inspect_err(|e| {
                tracing::error!("failed to collect trace data: {}", e);
            });
            if guard.state == State::Done {
                break;
            }
            drop(tracer.state_cv.wait(guard).unwrap());
        }
    }
    tracer.state.lock().unwrap().nr_wakes = nr_wakes;
}

/// Arm sampling on every thread that should be sampled, and snapshot the load map of any
/// compartment newly seen.
///
/// With `--all-threads` this is every thread on the system: a build spends nearly all its time in
/// child compartments (each `rustc`) and in the servers those children call, none of which the
/// traced compartment's own thread list can reach.
/// What the 100 ms tick actually costs, in counts.
///
/// The kernel side of tracing is already measured and is nothing -- 19,162 events, 334 us of
/// `Instant::now()`, 14 spins on the enqueue lock for a whole build. The consumer is the half that
/// was never counted, and both of its per-tick passes are O(N): [`arm`] walks every thread in the
/// system and re-snapshots every compartment ever seen, and [`resolve_slots`] rescans every event
/// collected so far, so the second is quadratic in total events over a run.
pub mod tickstats {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub static TICKS: AtomicUsize = AtomicUsize::new(0);
    pub static THREADS: AtomicUsize = AtomicUsize::new(0);
    pub static SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
    pub static LIBS: AtomicUsize = AtomicUsize::new(0);
    pub static SCANS: AtomicUsize = AtomicUsize::new(0);

    pub fn bump(c: &AtomicUsize, n: usize) {
        c.fetch_add(n, Relaxed);
    }

    pub fn report() -> String {
        format!(
            "{} ticks: {} thread arms ({} syscalls), {} compartment snapshots covering {} libs, {} event rescans",
            TICKS.load(Relaxed),
            THREADS.load(Relaxed),
            THREADS.load(Relaxed) * 2,
            SNAPSHOTS.load(Relaxed),
            LIBS.load(Relaxed),
            SCANS.load(Relaxed),
        )
    }
}

/// What the tick has already done, so it does not do it again.
///
/// Every field here exists because a tick was repeating work: 300 ticks of a 30 s build issued
/// 37,506 thread-arming syscalls and 6,236 compartment snapshots covering 36,029 library
/// `info()` calls -- each of the latter a monitor gate call, and therefore two
/// `sys_thread_set_active_sctx_id` syscalls -- against 20,094 trace events actually collected.
/// The profiler was doing an order of magnitude more syscall work than the thing it measured, and
/// the resulting gate traffic is what made `sys_thread_set_active_sctx_id` the top hot spot in its
/// own profile.
#[derive(Default)]
pub struct ArmState {
    /// Security contexts seen on any thread, so their compartments get snapshotted.
    seen: Vec<ObjID>,
    /// Threads already told to sample. Arming is idempotent and sticky, so doing it twice is pure
    /// syscall.
    armed: HashSet<ObjID>,
    /// Compartments whose load map has stopped growing. Re-snapshotting exists to catch a
    /// compartment first seen mid-load, which is a transient -- once the map stops growing there
    /// is nothing further to learn, and a short-lived compartment stops being looked up at all
    /// once it can no longer be resolved.
    stable: HashSet<ObjID>,
    /// Entries `resolve_slots` has already examined. The scan is over everything collected so far,
    /// so without a cursor it is quadratic in event count: 2,941,622 rescans over one build.
    scanned: usize,
    /// Ticks so far, so per-thread cpu accounting can run at a lower rate than arming.
    ticks: usize,
}

fn arm(cli: &Cli, comp: &CompartmentHandle, tracer: &Tracer, st: &mut ArmState) {
    if !cli.prog.all_threads {
        for thread in comp.threads() {
            if st.armed.insert(thread.repr_id) {
                let _ = sys_thread_set_trace_events(thread.repr_id, PERTHREAD_TRACE_GEN_SAMPLE);
            }
        }
        return;
    }
    tickstats::bump(&tickstats::TICKS, 1);
    st.ticks += 1;
    // Sampling is driven from `needs_reschedule`, so a thread that wakes and blocks constantly
    // gets a sampling opportunity per pass while a compute-bound one gets few -- sample share
    // measures scheduling activity as much as cpu. `ThreadSchedStats` is tick-based and immune to
    // that, so it is the arbiter for "which thread actually used the cpu". Read at 1 Hz rather
    // than per tick: this is one syscall per thread and the whole point of the memoing above was
    // to stop doing that 300 times.
    let read_stats = st.ticks % 10 == 1;
    let mut buf = [ObjID::default(); 128];
    let mut offset = 0;
    loop {
        let Ok(count) = sys_enumerate(EnumerateKind::Threads, &mut buf, offset) else {
            break;
        };
        if count == 0 {
            break;
        }
        for id in &buf[0..count] {
            // Arming is sticky, so only a thread we have not armed needs the syscall. The sctx
            // read stays unconditional: a thread's *active* context changes as it makes
            // cross-compartment calls, and that is how new compartments are discovered.
            if st.armed.insert(*id) {
                tickstats::bump(&tickstats::THREADS, 1);
                let _ = sys_thread_set_trace_events(*id, PERTHREAD_TRACE_GEN_SAMPLE);
            }
            if read_stats {
                let mut ss = ThreadSchedStats::default();
                if sys_thread_read_stats(*id, &mut ss).is_ok() {
                    tracer.state.lock().unwrap().thread_stats.insert(*id, ss);
                }
            }
            let mut ids = ThreadSctxIds::default();
            if sys_thread_read_sctx_ids(*id, &mut ids).is_err() {
                continue;
            }
            for sctx in [ids.home, ids.active] {
                if sctx.raw() != 0 && !st.seen.contains(&sctx) {
                    st.seen.push(sctx);
                }
            }
        }
        offset += count;
    }
    // Re-snapshot every tick rather than once per compartment. A compartment first seen while the
    // monitor is still loading it yields a partial map, and a short-lived one (each rustc) is never
    // seen again -- which left every pc in a build unresolvable against a map missing its slots.
    for sctx in st.seen.iter() {
        if st.stable.contains(sctx) {
            continue;
        }
        if !snapshot_comp(*sctx, tracer) {
            st.stable.insert(*sctx);
        }
    }
}

/// Record `sctx -> (name, load map)`, keeping the fullest map ever seen for that compartment.
///
/// Must happen while the compartment is alive: by report time every child has exited and its
/// libraries can no longer be located. Keeping the fullest map (rather than the first) is what
/// makes a short-lived compartment symbolizable -- the first sighting usually catches it before
/// the monitor has finished loading its libraries.
/// Returns whether this compartment is still worth re-snapshotting: false once its map has stopped
/// growing, or once it can no longer be looked up at all.
fn snapshot_comp(sctx: ObjID, tracer: &Tracer) -> bool {
    tickstats::bump(&tickstats::SNAPSHOTS, 1);
    let Ok(handle) = CompartmentHandle::lookup_id(sctx) else {
        return false;
    };
    let Ok(info) = handle.info() else {
        return false;
    };
    let name = info.name.to_string();
    let libs: Vec<_> = handle
        .libs()
        .map(|l| {
            let i = l.info();
            LoadedLib {
                name: i.name.clone(),
                start: i.start as usize,
                len: i.len,
                objid: i.objid,
            }
        })
        .collect();
    tickstats::bump(&tickstats::LIBS, libs.len());
    let mut state = tracer.state.lock().unwrap();
    match state.comp_maps.iter_mut().find(|c| c.sctx == sctx) {
        Some(existing) => {
            if libs.len() > existing.libs.len() {
                existing.libs = libs;
                existing.name = name;
                return true;
            }
            false
        }
        None => {
            state.comp_maps.push(CompMap {
                sctx,
                name,
                libs,
                _keepalive: handle,
            });
            true
        }
    }
}

/// Resolve the slot of every sample seen so far to the object mapped there, while it still is.
fn resolve_slots(tracer: &Tracer, st: &mut ArmState) {
    let mut state = tracer.state.lock().unwrap();
    let mut want: Vec<usize> = Vec::new();
    let mut scanned = 0usize;
    // Skip what earlier ticks already examined. The underlying iterator still walks those entries,
    // but the per-entry work -- a cast, four slot computations, a map lookup and a linear
    // `want.contains` each -- is what made this quadratic.
    for (head, data) in state.data().skip(st.scanned) {
        scanned += 1;
        if head.kind != TraceKind::Thread || head.event & THREAD_SAMPLE == 0 {
            continue;
        }
        let Some(d) = data.and_then(|d| d.try_cast::<ThreadSamplingEvent>(THREAD_SAMPLE)) else {
            continue;
        };
        // Not just the code slot: `di` is where a `rep stos` leaf is writing, and naming that
        // object is the whole point of capturing it. Slots must be resolved while the run is
        // live -- by report time every rustc has exited and its slots resolve to nothing.
        for addr in [d.data.ip, d.data.sp, d.data.bp, d.data.di] {
            let slot = (addr >> 30) as usize;
            if slot != 0 && !state.slot_map.contains_key(&slot) && !want.contains(&slot) {
                want.push(slot);
            }
        }
    }
    tickstats::bump(&tickstats::SCANS, scanned);
    st.scanned += scanned;
    for slot in want {
        if let Ok(mi) = sys_object_read_map(None, slot) {
            state.slot_map.insert(slot, mi.id);
        }
    }
}

pub fn start(
    cli: &Cli,
    comp: CompartmentHandle,
    specs: Vec<TraceSpec>,
    rt_trace: Option<Object<BaseWrap>>,
) -> miette::Result<TracingState> {
    let state = TracingState::new(comp.info().unwrap().name, specs.as_slice(), rt_trace)?;

    let tracer = Tracer {
        state: Mutex::new(state),
        specs,
        state_cv: Condvar::new(),
        notifier: AtomicU64::new(0),
    };
    std::thread::scope(|scope| {
        let th_collector = Builder::new()
            .name("trace-collector".to_owned())
            .spawn_scoped(scope, || collector(&tracer))
            .into_diagnostic()?;

        tracer.wait_for(State::Ready);

        let start = Instant::now();
        // Capture the load map before the target can exit: `comp.libs()` needs a live compartment,
        // and without it the PC histogram cannot be symbolized against a known binary.
        {
            let map: Vec<_> = comp
                .libs()
                .map(|l| {
                    let i = l.info();
                    LoadedLib {
                        name: i.name.clone(),
                        start: i.start as usize,
                        len: i.len,
                        objid: i.objid,
                    }
                })
                .collect();
            tracer.state.lock().unwrap().load_map = map;
        }
        for thread in comp.threads() {
            let id: ObjID = thread.repr_id;
            tracing::debug!("resuming compartment thread {}", id);
            sys_thread_change_state(id, ExecutionState::Running).into_diagnostic()?;
            if cli.prog.sample {
                tracing::debug!("setting per-thread sampling for {}", id);
                sys_thread_set_trace_events(id, PERTHREAD_TRACE_GEN_SAMPLE).into_diagnostic()?;
            }
        }
        let mut arm_state = ArmState::default();
        tracer.set_state(State::Running);

        // With a timeout, stop collecting and report even if the target never exits -- the whole
        // point is profiling a wedged compartment. The comp handle's later drop tears it down.
        let deadline = cli
            .prog
            .timeout
            .map(|secs| start + std::time::Duration::from_secs(secs));
        let mut flags = comp.info().unwrap().flags;
        // Sampling needs a tick even without a timeout: it does not inherit across spawns, so
        // threads created after startup -- codegen workers, and every child compartment a build
        // spawns -- would otherwise never be armed.
        let poll = cli.prog.sample || deadline.is_some();
        while !flags.contains(CompartmentFlags::EXITED) {
            if poll {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    tracing::info!("timeout reached with target still running; reporting anyway");
                    break;
                }
                // `comp.wait` has no timeout of its own; poll on a coarse tick instead.
                std::thread::sleep(std::time::Duration::from_millis(100));
                if cli.prog.sample {
                    arm(cli, &comp, &tracer, &mut arm_state);
                    resolve_slots(&tracer, &mut arm_state);
                }
                flags = comp.info().unwrap().flags;
            } else {
                flags = comp.wait(flags);
            }
        }
        let end = Instant::now();
        tracing::debug!(
            "compartment exited after {:2.2}s",
            (end - start).as_secs_f32()
        );
        tracer.state.lock().unwrap().start_time = start;
        tracer.state.lock().unwrap().end_time = end;

        tracer.set_state(State::Done);
        tracer.notify_exit();

        th_collector.join().unwrap();

        std::io::Result::Ok(()).into_diagnostic()
    })?;
    tracer.state.into_inner().into_diagnostic()
}
