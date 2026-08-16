//! The scheduler is currently based off of FreeBSD's ULE scheduler. It maintains a per-CPU runqueue
//! where next tasks are selected from. If no tasks are available, the CPU tries to steal one from
//! another CPU. Tasks are periodically balanced between cores by moving them between the most
//! loaded and least loaded core.
//!
//! The runqueues are organized into three parts by priority class. Realtime and Interrupt priority
//! tasks are placed in the realtime queue, User priority tasks are placed in the timeshare queue,
//! and Idle and Background tasks are placed in the Background queue. Both the realtime and
//! background queues are simple arrays (indexed by priority) of FIFO task lists. When selecting
//! from the runqueue, a CPU first tries to take from the realtime queue, then from the timeshare
//! queue, and then from the background queue. If all queue are empty (and the CPU failed to steal a
//! thread) the CPU runs the per-cpu idle task. The idle task is never in the runqueue. When
//! running, a thread is not in a runqueue, though the CPU tracks the current thread.
//!
//! The timeshare queue is a calendar queue. Incoming tasks are placed into the queue based on
//! their priority and the current insert marker (with higher priority tasks being placed closer to
//! the insert marker). The queue is circular, with threads being removed from the removal marker by
//! CPUs trying to get a next task.
//!
//! Each entry in the queue is a linked list of tasks, and removal takes from the list at the
//! current removal marker until it is empty. Once an entry is empty, the removal marker is advanced
//! up to the insert marker or to a non-empty entry. The insert marker is advanced on the scheduler
//! tick, and the removal marker is advanced on clock hardticks if possible.
//!
//! Threads get a timeslice based on the maximum timeslice, their priority, and the status of the
//! runqueue (and priority class sub-queue) that they inhabit. Currently, time is divided evenly
//! between timeshare tasks. Additionally, tasks are assigned a deadline, which, if expired, allows
//! timeshare tasks to jump to the lowest priority realtime queue slot to ensure low-latency for
//! tasks that have slept. Timeshare and deadline calculation and effects are currently a work in
//! progress, and will need tuning.
//!
//! A thread's priority is affected by both its base priority and its donated priority. Tasks that
//! need to wait for another thread (e.g. in a mutex) donate their priority to the thread they are
//! waiting on to prevent priority inversion.

use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering},
    u64,
};

use bitset_core::BitSet;
use twizzler_abi::{
    object::ObjID,
    thread::ExecutionState,
    trace::{SwitchFlags, ThreadCtxSwitch, ThreadMigrate, TraceEntryFlags, TraceKind},
};

pub const MAX_TIMESLICE_TICKS: u32 = 100;
pub const MIN_TIMESLICE_TICKS: u32 = 2;
pub const DEFAULT_TIMESLICE_TICKS: u32 = 32;

use super::{
    mp::{current_processor, get_processor},
    rq::RunQueue,
};
use crate::{
    clock::{Nanoseconds, get_current_ticks},
    interrupt,
    once::Once,
    processor::{Processor, mp::MAX_CPU_ID},
    spinlock::Spinlock,
    thread::{Thread, ThreadRef, current_thread_ref, priority::Priority, set_current_thread},
    trace::{
        mgr::{TRACE_MGR, TraceEvent, is_thread_ktrace_thread},
        new_trace_entry_thread,
    },
    utils::quick_random,
};

#[derive(Clone, Debug, Copy)]
pub enum CPUTopoType {
    System,
    Cache,
    Thread,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct CpuSet {
    set: [u64; MAX_CPU_ID / 64],
}

impl CpuSet {
    pub fn all() -> Self {
        let mut set = [0xffffffffffffffff; MAX_CPU_ID / 64];
        set.bit_init(true);
        Self { set }
    }

    pub fn empty() -> Self {
        let mut set = [0; MAX_CPU_ID / 64];
        set.bit_init(false);
        Self { set }
    }

    pub fn insert(&mut self, id: u32) {
        self.set.bit_set(id as usize);
    }

    pub fn remove(&mut self, id: u32) {
        self.set.bit_reset(id as usize);
    }

    pub fn contains(&self, id: u32) -> bool {
        self.set.bit_test(id as usize)
    }

    pub fn union_with(&mut self, other: &Self) {
        for (a, b) in self.set.iter_mut().zip(other.set.iter()) {
            *a |= *b;
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.set.bit_any()
    }
}

#[derive(Debug)]
pub struct CPUTopoNode {
    level_type: CPUTopoType,
    count: usize,
    cpuset: CpuSet,
    first: u32,
    last: u32,
    children: Vec<CPUTopoNode>,
    parent: AtomicPtr<CPUTopoNode>,
}

impl CPUTopoNode {
    pub fn new(ty: CPUTopoType) -> CPUTopoNode {
        Self {
            cpuset: CpuSet::empty(),
            first: u32::MAX,
            last: 0,
            children: alloc::vec![],
            parent: AtomicPtr::new(core::ptr::null_mut()),
            level_type: ty,
            count: 0,
        }
    }

    pub fn child(&self, child: usize) -> Option<&CPUTopoNode> {
        self.children.get(child)
    }

    pub fn child_mut(&mut self, child: usize) -> Option<&mut CPUTopoNode> {
        self.children.get_mut(child)
    }

    pub fn add_child(&mut self, path: usize, mut node: CPUTopoNode) {
        self.children
            .resize_with(core::cmp::max(path + 1, self.children.len()), || {
                CPUTopoNode::new(CPUTopoType::Other)
            });
        node.parent = AtomicPtr::new(self);
        self.children[path] = node;
    }

    pub fn parent(&self) -> Option<&CPUTopoNode> {
        unsafe { self.parent.load(Ordering::SeqCst).as_ref() }
    }

    pub fn set_cpu(&mut self, id: u32) {
        self.cpuset.insert(id);
        if self.first > id {
            self.first = id;
        }
        if self.last < id {
            self.last = id;
        }
        self.count += 1;
    }

    pub fn find_cpu(&self, id: u32) -> Option<&CPUTopoNode> {
        if !self.cpuset.contains(id) {
            return None;
        }

        if self.children.is_empty() {
            return Some(self);
        }

        for child in &self.children {
            if let Some(node) = child.find_cpu(id) {
                return Some(node);
            }
        }
        None
    }
}

static CPU_TOPOLOGY_ROOT: Once<Box<CPUTopoNode>> = Once::new();

pub fn set_cpu_topology(root: Box<CPUTopoNode>) {
    CPU_TOPOLOGY_ROOT.call_once(|| root);
}

pub fn get_cpu_topology() -> &'static CPUTopoNode {
    &*CPU_TOPOLOGY_ROOT.poll().unwrap()
}

struct SearchCPUResult {
    load: u64,
    cpuid: u32,
}

#[track_caller]
fn find_cpu_from_topo(
    node: &CPUTopoNode,
    highest: bool,
    pri: Option<&Priority>,
    allowed_set: Option<&CpuSet>,
) -> Option<SearchCPUResult> {
    let mut best = if highest { 0 } else { u64::MAX };
    let mut best_cpu = None;
    if !node.children.is_empty() {
        for n in 0..node.children.len() {
            /* TODO: maybe we could optimize here by pruning based on allowed_set */
            let res = find_cpu_from_topo(node.child(n).unwrap(), highest, pri, allowed_set);
            if let Some(res) = res {
                if highest {
                    if res.load > best || best_cpu.is_none() {
                        best_cpu = Some(res.cpuid);
                        best = res.load;
                    }
                } else if res.load < best || best_cpu.is_none() {
                    best_cpu = Some(res.cpuid);
                    best = res.load;
                }
            }
        }
        best_cpu.map(|c| SearchCPUResult {
            load: best,
            cpuid: c,
        })
    } else {
        for c in node.first..=node.last {
            if node.cpuset.contains(c) {
                let processor = get_processor(c as u32);
                let skip = pri.map_or(false, |pri| &processor.current_priority() > pri)
                    || allowed_set.map_or(false, |set| !set.contains(c));
                if skip {
                    continue;
                }
                let load = processor.current_load();
                log::trace!(
                    "{} {} {:?}: cpu {} considering {}: load {},{},{}",
                    core::panic::Location::caller(),
                    highest,
                    pri,
                    current_processor().id,
                    processor.id,
                    processor.current_load(),
                    processor.rq.current_load(),
                    processor.rq.current_timeshare_load(),
                );
                /* jitter. This is similar to how freebsd does things */
                let jload = (load * 256).saturating_sub((quick_random() % 128) as u64);
                if highest {
                    if jload > best || best_cpu.is_none() {
                        best_cpu = Some(c as u32);
                        best = jload;
                    }
                } else if jload < best || best_cpu.is_none() {
                    best_cpu = Some(c as u32);
                    best = jload;
                }
            }
        }
        best_cpu.map(|c| SearchCPUResult {
            load: best,
            cpuid: c,
        })
    }
}

fn choose_cpu_steal_via_topo(node: &CPUTopoNode, allowed_set: &mut CpuSet) -> Option<u32> {
    if allowed_set.is_empty() {
        return None;
    }
    // Walk up the topology, checking nearby CPUs for extra load. After we check a CPU, remove it
    // from the allowed_set to avoid rechecking it in the future.
    for c in node.first..=node.last {
        if node.cpuset.contains(c) {
            if !allowed_set.contains(c) {
                continue;
            }
            let processor = get_processor(c);
            let load = processor.current_load();
            if load >= STEAL_LOAD_THRESH && processor.rq.movable() > 0 {
                return Some(processor.id);
            }
            allowed_set.remove(c);
        }
    }
    choose_cpu_steal_via_topo(node.parent()?, allowed_set)
}

struct BalanceResult {
    donor: u32,
    recipient: u32,
}

fn choose_cpu_balance(node: &CPUTopoNode, allowed_set: &CpuSet) -> Option<BalanceResult> {
    if allowed_set.is_empty() {
        return None;
    }
    // Walk up the topology, checking nearby CPUs for highest and lowest loaded.
    let mut lowest_load = (u32::MAX, u64::MAX);
    let mut highest_load = (u32::MIN, u64::MIN);

    for c in node.first..=node.last {
        if node.cpuset.contains(c as u32) {
            if !allowed_set.contains(c as u32) {
                continue;
            }
            let processor = get_processor(c as u32);
            let load = processor.current_load();
            if load == 0 {}
            // Use some jitter.
            let jload = (load * 256).saturating_sub((quick_random() % 128) as u64);

            if jload > highest_load.1 {
                highest_load = (processor.id, jload);
            } else if jload < lowest_load.1 {
                lowest_load = (processor.id, jload);
            }
        }
    }
    if lowest_load.0 != u32::MAX && lowest_load.0 != highest_load.0 {
        return Some(BalanceResult {
            donor: highest_load.0,
            recipient: lowest_load.0,
        });
    }
    None
}

fn reset_thread_time(thread: &ThreadRef, processor: &Processor) {
    thread.sched.set_deadline(
        get_current_ticks() + processor.rq.deadline(thread.effective_priority().class),
    );
    thread.sched.reset_timeslice();
}

fn schedule_thread_on_cpu(thread: ThreadRef, processor: &Processor, is_current: bool) {
    if thread.is_exiting() {
        return;
    }
    let is_remote = processor.id != current_processor().id;
    let outranks_target =
        processor.rq.is_empty() || processor.rq.current_priority() <= thread.effective_priority();
    let should_signal = is_remote && outranks_target;
    let woken_priority = thread.effective_priority();

    // Classified before the insert moves `thread`, and stamped on it so the latency can be read
    // when it actually reaches a cpu (`switch_to`). Whole-boot ratios could not attribute the
    // 3-4 stalls that make every mean in `schedtime.md`; this is per wake.
    //
    // A reinsertion is not a wake, and excluding it is load-bearing twice over. `do_schedule`
    // routes the *current* thread back through here with `is_current = false` on the REINSERT
    // path, so without this check (a) every preemption-driven reinsertion was stamped and
    // counted as a wake, which polluted the histogram, and (b) under the `>=` below a
    // reinserted thread compares equal to *itself* and marks preempt, so each preemption
    // produces another. That self-sustaining loop is what the first `>=` attempt actually
    // measured -- 6820 marks against 1129, info pickup 343-467 -> 657-713 us -- rather than the
    // equal-priority thrash it was blamed on.
    let is_reinsertion = current_thread_ref().is_some_and(|cur| cur.id() == thread.id());
    // Strict `>`, and the priority boundary is *not* the lever. `>=` was tried on top of the
    // reinsertion fix -- matching what `needs_reschedule` does at a tick -- and it moved ~240 wakes
    // from `lost-pri` into `marked` without moving their latency: `lost-pri` shed 47 stalls over
    // 1 ms, `marked` gained 33, and info pickup (396-469 -> 451-461 us), `lookup_object_and_wait`
    // (836-875 -> 815-839 us) and `pagepar` (63 -> 64 ms) were all flat. Marking preempt does not
    // make a stalled wake fast; the earlier "marked 20 us vs lost 270 us" split was selection, not
    // causation. Reverted as the smaller change with no measured benefit.
    let kind = if is_current || is_reinsertion {
        0
    } else if is_remote {
        wakestats::WAKE_REMOTE
    } else {
        match current_thread_ref() {
            Some(cur) if cur.is_idle_thread() => wakestats::WAKE_LOCAL_IDLE,
            Some(cur) if woken_priority > cur.effective_priority() => wakestats::WAKE_LOCAL_MARKED,
            Some(_) => wakestats::WAKE_LOCAL_LOST,
            None => 0,
        }
    };
    if kind != 0 {
        thread.sched.wake_ticks.store(
            crate::instant::Instant::now().raw_ticks().max(1),
            Ordering::Relaxed,
        );
        thread.sched.wake_kind.store(kind, Ordering::Relaxed);
    }

    thread.sched.moving_to_queue(processor.id);
    reset_thread_time(&thread, processor);
    processor.rq.insert(thread, is_current);

    if is_remote {
        wakestats::remote(should_signal);
    }
    if should_signal {
        processor.wakeup(true);
        return;
    }
    if is_remote {
        return;
    }
    // A wake onto *this* cpu used to end here: inserted on the run queue and nothing told the
    // running thread about it. `should_signal` is false for every local wake by construction, and
    // `schedule_mark_preempt` has no other caller on any wake path -- so the woken thread waited
    // for `schedule_hardtick` to notice it, and only then if its priority still won. That is a
    // millisecond at best (one tick) and a whole timeslice when it does not win the tick's
    // `rq_pri >= cur_pri` test, against hand-offs whose median is tens of microseconds.
    //
    // At smp1 that is *every* wake in the system, which is why `schedtime.md` measures the pager's
    // lane pickup at 372-456 us there while the same hop costs 25-36 us at smp4.
    //
    // Marked rather than switched: this runs inside the waker's critical section on most paths
    // (`Request::signal`, `requeue_all`), where switching is forbidden. The flag is consumed at the
    // next interrupt return, which `schedule_maybe_preempt` now defers if we are still critical.
    //
    // `is_current` excluded: that is `schedule` reinserting the thread it is already running, not a
    // wake, and marking preempt for it would ask the scheduler to preempt in favour of itself.
    match kind {
        // Waking anything while this cpu is *idling* must preempt, and there is no priority
        // question to ask: the idle thread has no work and nothing to protect.
        // `schedule_resched` -- the ipi handler -- already says exactly this (`if is_idle
        // || needs_reschedule(false)`), but no local wake reached it, so an idling cpu sat
        // until the next tick with a runnable thread beside it. 367-370 wakes a boot at
        // smp1, measured at ~400 us mean with a 144-146 ms outlier every run:
        // the worst latencies anywhere in `schedtime.md`'s data, and the only class where the delay
        // has no candidate explanation other than "nobody said to stop idling".
        wakestats::WAKE_LOCAL_IDLE => {
            wakestats::local(false, true);
            schedule_mark_preempt();
        }
        wakestats::WAKE_LOCAL_MARKED => {
            wakestats::local(true, false);
            schedule_mark_preempt();
        }
        wakestats::WAKE_LOCAL_LOST => wakestats::local(false, false),
        _ => {}
    }
}

fn take_a_thread_from_cpu(processor: &Processor, new_cpu_rq: u32) -> Option<ThreadRef> {
    if let Some(th) = processor.rq.take(new_cpu_rq != processor.id) {
        th.sched.moving_to_queue(new_cpu_rq);
        Some(th)
    } else {
        None
    }
}

const STEAL_LOAD_THRESH: u64 = 2;
#[track_caller]
fn try_steal() -> Option<ThreadRef> {
    /* TODO: we need a cooldown on migration */
    let us = current_processor();
    //let res = find_cpu_from_topo(get_cpu_topology(), true, None, None);
    let our_topo_node = get_cpu_topology().find_cpu(us.id).unwrap();
    let mut allowed_set = get_cpu_topology().cpuset.clone();
    allowed_set.remove(us.id);
    if let Some(cpuid) = choose_cpu_steal_via_topo(our_topo_node, &mut allowed_set) {
        if !us.rq.is_empty() {
            return us.rq.take(false);
        }
        let processor = get_processor(cpuid);
        let otherload = processor.current_load();
        if otherload >= STEAL_LOAD_THRESH {
            let thread = take_a_thread_from_cpu(processor, us.id);
            if thread.is_some() {
                log::trace!(
                    "stole {} ({} -> {}): {} {}",
                    thread.as_ref().unwrap().id(),
                    processor.id,
                    us.id,
                    otherload,
                    us.current_load()
                );
            }
            return thread;
        }
    }
    None
}

/// Set while a rebalance is in progress. Try-and-skip rather than a lock: this runs from the
/// statclock interrupt handler, and the loop below is up to `MAX_STEPS` topology searches and
/// thread migrations -- so a second caller arriving mid-pass has nothing to gain by waiting for
/// the first to finish. Its own balance would start from a set of loads that the pass it waited on
/// has already changed, and it waits for that with interrupts masked. Skipping costs one rebalance
/// interval, which is exactly the granularity this decision is made at anyway.
static BALANCING: AtomicBool = AtomicBool::new(false);

/// Clears [BALANCING] on every exit from `balance`, including a panic: a leaked flag silently
/// disables rebalancing for the rest of the boot.
struct BalanceGuard;

impl Drop for BalanceGuard {
    fn drop(&mut self) {
        BALANCING.store(false, Ordering::Release);
    }
}

fn balance(topo: &CPUTopoNode) {
    if BALANCING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let _guard = BalanceGuard;
    log::trace!("starting rebalance at {}", get_current_ticks());

    let mut allowed_set = topo.cpuset;
    const MAX_STEPS: usize = 20;
    let mut steps = 0;
    while steps < MAX_STEPS {
        if let Some(result) = choose_cpu_balance(get_cpu_topology(), &allowed_set) {
            let donor = get_processor(result.donor);
            let recipient = get_processor(result.recipient);
            if donor.current_load() == 0 {
                break;
            }

            log::trace!(
                "considering {} -> {} (loads {} {})",
                donor.id,
                recipient.id,
                donor.current_load(),
                recipient.current_load(),
            );

            donor.set_rebalance();
            if donor.rq.current_load() > 0 {
                allowed_set.remove(result.recipient);
                let thread = take_a_thread_from_cpu(donor, recipient.id);
                if let Some(thread) = thread {
                    log::trace!(
                        "rebalanced {} ({} -> {})",
                        thread.id(),
                        donor.id,
                        recipient.id
                    );
                    schedule_thread_on_cpu(thread, recipient, false);
                    steps += 10;
                }
            } else if donor.current_load() == 1 {
                allowed_set.remove(result.donor);
            }
        }
        steps += 1;
    }
}

fn select_cpu(thread: &ThreadRef, try_avoid: Option<u32>) -> u32 {
    /* TODO: restrict via cpu sets as step 0, and in global searches */
    /* TODO: take SMT into acount */
    let last_cpuid = thread
        .sched
        .preferred_cpu()
        .map(|(x, _p)| x as i32)
        .unwrap_or(-1);
    /* 1: if the thread can run on the last CPU it ran on, and that CPU is idle, then do that. */
    if last_cpuid >= 0 && try_avoid.is_none_or(|ta| ta != last_cpuid as u32) {
        let processor = get_processor(last_cpuid as u32);
        if processor.rq.current_load() == 0 {
            return last_cpuid as u32;
        }
        if thread.effective_priority() > processor.current_priority() {
            return last_cpuid as u32;
        }
    }

    /* 2: search for the least loaded that will run this thread immediately */
    let res = find_cpu_from_topo(
        get_cpu_topology(),
        false,
        Some(&thread.effective_priority()),
        None,
    );
    if let Some(res) = res {
        if try_avoid.is_none_or(|ta| ta != res.cpuid) {
            return res.cpuid;
        }
    }

    /* 3: search for the least loaded */
    let res = find_cpu_from_topo(get_cpu_topology(), false, None, None)
        .expect("global CPU search should always produce results");

    res.cpuid
}

static ALL_THREADS: Spinlock<BTreeMap<u64, ThreadRef>> = Spinlock::new(BTreeMap::new());
static ALL_THREADS_REPR: Spinlock<BTreeMap<ObjID, ThreadRef>> = Spinlock::new(BTreeMap::new());

pub fn with_all_threads<F>(mut f: F)
where
    F: FnMut(&BTreeMap<u64, ThreadRef>),
{
    let guard = ALL_THREADS.lock();
    f(&guard);
}

pub fn with_each_thread<F>(mut f: F)
where
    F: FnMut(&ThreadRef),
{
    let guard = ALL_THREADS.lock();
    for (_, th) in guard.iter() {
        f(th);
    }
}

pub fn remove_thread(id: u64) {
    if let Some(t) = ALL_THREADS.lock().remove(&id) {
        ALL_THREADS_REPR
            .lock()
            .remove(&t.control_object.object().id());
    }
}

pub fn lookup_thread_repr(id: ObjID) -> Option<ThreadRef> {
    ALL_THREADS_REPR.lock().get(&id).cloned()
}

pub fn schedule_new_thread(thread: Thread) -> ThreadRef {
    thread.set_state(ExecutionState::Running);
    let thread = Arc::new(thread);
    {
        ALL_THREADS.lock().insert(thread.id(), thread.clone());
        ALL_THREADS_REPR
            .lock()
            .insert(thread.control_object.object().id(), thread.clone());
    }
    *unsafe { thread.self_reference.get().as_mut().unwrap() } =
        Box::into_raw(Box::new(thread.clone()));
    let cpuid = select_cpu(&thread, None);
    let processor = get_processor(cpuid);
    schedule_thread_on_cpu(thread.clone(), processor, false);
    thread
}

#[track_caller]
pub fn schedule_thread(thread: ThreadRef) {
    // Checked before the state write, not after it in `schedule_thread_on_cpu`. That order is what
    // `Mutex::lock`'s dead-handoff reclaim depends on: it recognizes an owner that will never take
    // the mutex by its `Exited` state, and a `set_state(Running)` here overwrites exactly that --
    // leaving the mutex owned forever by a dead thread, with every later locker asleep behind it
    // and no path able to notice.
    if thread.is_exiting() {
        return;
    }
    thread.set_state(ExecutionState::Running);
    if thread.is_idle_thread() {
        return;
    }
    let cpuid = select_cpu(&thread, None);
    let processor = get_processor(cpuid);
    log::trace!(
        "{} on {} (load = {},{}): picked {} (load = {},{}) for thread {}",
        core::panic::Location::caller(),
        current_processor().id,
        current_processor().current_load(),
        current_processor().rq.current_load(),
        cpuid,
        processor.current_load(),
        processor.rq.current_load(),
        thread.id()
    );
    schedule_thread_on_cpu(thread, processor, false);
}

pub fn create_idle_thread() {
    let idle = Arc::new(Thread::new_idle());
    *unsafe { idle.self_reference.get().as_mut().unwrap() } = Box::into_raw(Box::new(idle.clone()));
    current_processor().set_idle_thread(idle.clone());
    unsafe { set_current_thread(&idle) };
}

fn trace_migrate(th: &ThreadRef, from: u64, to: u64) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_MIGRATE) {
        let data = ThreadMigrate { from, to };
        let entry = new_trace_entry_thread(
            th,
            current_processor().id as u64,
            TraceKind::Thread,
            twizzler_abi::trace::THREAD_MIGRATE,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, data));
    }
}

fn trace_switch(from: &ThreadRef, to: &ThreadRef, sflags: SchedFlags) {
    if TRACE_MGR.any_enabled(
        TraceKind::Thread,
        twizzler_abi::trace::THREAD_CONTEXT_SWITCH,
    ) {
        let mut flags = SwitchFlags::empty();
        if is_thread_ktrace_thread(to) {
            flags.insert(SwitchFlags::IS_TRACE);
        }
        if sflags.contains(SchedFlags::PREEMPT) {
            flags.insert(SwitchFlags::PREEMPTED);
        }
        if to.is_idle_thread() {
            flags.insert(SwitchFlags::TO_IDLE);
        }
        if !to.is_in_user() {
            flags.insert(SwitchFlags::TO_KTHREAD);
        }
        if !sflags.contains(SchedFlags::REINSERT) {
            flags.insert(SwitchFlags::SLEEPING);
        }
        let data = ThreadCtxSwitch {
            to: Some(to.objid()),
            flags,
        };
        let entry = new_trace_entry_thread(
            from,
            current_processor().id as u64,
            TraceKind::Thread,
            twizzler_abi::trace::THREAD_CONTEXT_SWITCH,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, data));
    }
}

fn switch_to(thread: ThreadRef, old: &ThreadRef, flags: SchedFlags) {
    let cp = current_processor();
    // Close out the wake stamp: this is the one place a thread becomes the running thread, so the
    // interval from `schedule_thread_on_cpu` to here is exactly wake-to-run. Taken rather than
    // read, so a thread that is switched to again without an intervening wake is not counted
    // twice.
    let wake_ticks = thread.sched.wake_ticks.swap(0, Ordering::Relaxed);
    if wake_ticks != 0 {
        let kind = thread.sched.wake_kind.swap(0, Ordering::Relaxed);
        wakestats::wake_to_run(
            kind,
            crate::instant::Instant::now().ns_since_ticks(wake_ticks),
        );
    }
    let oldcpu = thread.sched.moving_to_active(cp.id);
    if old.id() != thread.id() {
        trace_switch(&old, &thread, flags);
    }
    cp.stats.switches.fetch_add(1, Ordering::Relaxed);

    if let Some(oldcpu) = oldcpu {
        if oldcpu != cp.id {
            log::trace!("migrated {} {} -> {}", thread.id(), oldcpu, cp.id);
            trace_migrate(&thread, oldcpu as u64, cp.id as u64);
        }
    }

    if !thread.is_idle_thread() {
        cp.current_priority
            .store(thread.effective_priority().raw(), Ordering::Release);
        cp.exit_idle();
        // TODO: we should probably reset the timer here based on rq and priority, but doing so
        // breaks tick counting on the BSP, so that will need to wait until we refactor ticking
        // to per-CPU.
        //crate::clock::schedule_oneshot_tick(cp.rq.timeslice(thread.effective_priority().class));
    } else {
        cp.enter_idle();
        cp.current_priority.store(0, Ordering::Release);
    }
    cp.reset_rebalance();
    crate::thread::locktrack::enter_switch_window();
    // Do NOT publish `thread` as current here. `do_schedule`'s REINSERT branch can already have
    // queued it on another cpu, so publishing before this cpu owns it makes two cpus report the
    // same current thread for the whole prologue -- the cross-cpu producer behind the stale lock
    // intents, the mutex_count underflow and the `maybe_suspend_self` identity assert. A thread
    // that has run before is published by `switch_thread` once `__do_switch` has won its
    // switch_lock. Waiting for that lock *here* would deadlock: `__do_switch` releases the
    // outgoing lock before acquiring the incoming one, and this would hold-and-wait.
    //
    // A thread that has never run is the exception, and must be published here: it is on exactly
    // one run queue and has never been current anywhere, so no second cpu can be calling it
    // current -- and it jumps straight to its entry point out of `__do_switch` rather than
    // returning into `switch_thread`.
    if !thread.mark_run() {
        unsafe { set_current_thread(&thread) };
    }

    // Release our strong ref before switching (into_raw + decrement keeps the pointer usable
    // afterward; the switch does not return on this path). Sound because the leaked
    // `Box<ThreadRef>` self-reference -- installed in schedule_new_thread/create_idle_thread and
    // reclaimed only by Processor::cleanup_exited once the thread has exited -- always holds
    // another strong ref.
    let threadt = Arc::into_raw(thread);
    unsafe {
        Arc::decrement_strong_count(threadt);
        threadt.as_ref().unwrap().switch_thread(old);
    }
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug)]
    pub struct SchedFlags: u32 {
        const REINSERT = 1;
        const YIELD = 2;
        const PREEMPT = 4;
    }
}

fn rq_has_higher<const N: usize>(thread: &ThreadRef, rq: &RunQueue<N>, eq: bool) -> bool {
    let th_pri = thread.effective_priority();
    let rq_pri = rq.current_priority();
    rq_pri > th_pri || (eq && rq_pri >= th_pri)
}

fn do_schedule(flags: SchedFlags) {
    let cur = current_thread_ref().unwrap();
    let processor = current_processor();

    if cur.is_exiting() {
        processor.push_exited(cur.clone());
    }

    if !cur.is_idle_thread() && flags.contains(SchedFlags::REINSERT) {
        // If we are re-inserting the thread, we may want to send it to another CPUs queue.
        // Check if either we were preempted (timeslice expired, or needed reschedule for another
        // reason) or if we have higher priority tasks to run. If so, look for a cpu to
        // insert the thread into. Otherwise just quickly reinsert it onto our queue so we
        // may choose it again soon.
        //
        // n.b. if we are yielding, we allow for equal-priority threads to count as "higher
        // priority" so that other threads can run if available. If all threads are truly
        // lower priority, yielding has less of an effect on timeshare threads.
        if flags.contains(SchedFlags::PREEMPT)
            || processor.must_rebalance()
            || rq_has_higher(cur, &processor.rq, flags.contains(SchedFlags::YIELD))
        {
            let cpuid = if processor.must_rebalance() {
                select_cpu(
                    &cur,
                    if processor.must_rebalance() {
                        Some(processor.id)
                    } else {
                        None
                    },
                )
            } else {
                processor.id
            };
            let processor = get_processor(cpuid);
            schedule_thread_on_cpu(cur.clone(), processor, false);
        } else {
            // This is a current thread to reinsert, but only count it as such if it is not
            // yielding so that other threads will run first.
            if flags.contains(SchedFlags::YIELD) {
                schedule_thread_on_cpu(cur.clone(), processor, false);
            } else {
                // shortcut -- we are intending to just run this thread again.
                reset_thread_time(cur, processor);
                processor.exit_idle();
                return;
            }
        }
    }

    let next = processor.rq.take(false);
    if let Some(next) = next {
        if &next == cur {
            // We took ourselves back off the queue, so we never reach switch_to (the only other
            // caller of moving_to_active). Clear current_processor_queue here, or we stay marked
            // as queued while actually running.
            cur.sched.moving_to_active(processor.id);
            return;
        }
        switch_to(next, cur, flags);
        return;
    }

    // No thread was found in our queue. Try to steal from another queue.
    if let Some(stolen) = try_steal() {
        let cp = current_processor();
        cp.stats.steals.fetch_add(1, Ordering::SeqCst);
        switch_to(stolen, cur, flags);
        return;
    }

    if cur.is_idle_thread() {
        return;
    } else {
        log::trace!(
            "{} idled from {} with load {}, flags {:?}",
            processor.id,
            cur.id(),
            processor.current_load(),
            flags
        );
    }
    switch_to(processor.idle_thread.wait().clone(), cur, flags);
}

pub fn schedule(flags: SchedFlags) {
    let cur = current_thread_ref().unwrap();
    /* TODO: if we preempt, just put the thread back on our list (or decide to not resched) */
    let istate = interrupt::disable();
    if cur.is_critical() {
        interrupt::set(istate);
        return;
    }

    // An idle thread must not lose its cpu while it holds a mutex. `do_schedule` deliberately never
    // reinserts one on a run queue, so it is not runnable-on-demand the way every other thread is:
    // it resumes only when its own cpu next finds nothing else to run. Descheduled mid-critical-
    // section it becomes a lock owner nothing can schedule, and the idle threads of the other cpus
    // then spin for that lock in `Mutex::lock` -- they do not sleep on it, and they donate no
    // priority to the owner, so there is no mechanism anywhere that gets the owner running again.
    //
    // The state is easy to reach and usually harmless: it shows up transiently in dozens of passing
    // runs, resolving as soon as the owner's cpu happens to go idle. When that cpu stays busy it
    // never resolves, and the run wedges with unbounded `mutex stall` reports naming an owner that
    // is `Running, runnable/off-cpu, rq -1, idle true`.
    //
    // Only involuntary preemption is refused. A voluntary block reaches here from `finish_blocking`
    // with the state already `Sleeping`, and that path must still be able to switch away -- an idle
    // thread takes it once per iteration of the very spin loop described above.
    if cur.is_idle_thread()
        && cur.get_mutex_count() > 0
        && cur.get_state() == ExecutionState::Running
    {
        interrupt::set(istate);
        return;
    }

    do_schedule(flags);
    interrupt::set(istate);

    if flags.contains(SchedFlags::REINSERT) {
        // Resolving the current thread and then suspending it must not straddle an
        // interrupt-enabled gap: a preemption in between changes who is current, and
        // `maybe_suspend_self` is only meaningful for the thread actually executing. `suspend()`
        // takes the same precaution for the same reason.
        interrupt::with_disabled(|| {
            if let Some(cur) = current_thread_ref() {
                cur.maybe_suspend_self();
            }
        });
        // Left outside: this can call `exit()`, which must not run with interrupts masked.
        if let Some(cur) = current_thread_ref() {
            cur.maybe_exit();
        }
    }
}

pub fn needs_reschedule(ticking: bool) -> bool {
    let processor = current_processor();
    let cur = {
        let cur = current_thread_ref();
        if cur.is_none() {
            return false;
        }
        cur.unwrap()
    };
    if cur.is_critical() {
        wakestats::resched(true);
        return false;
    }
    wakestats::resched(false);
    if cur.check_sampling() {
        return true;
    }
    if cur.must_suspend() {
        return true;
    }
    if processor.rq.is_empty() {
        return false;
    }
    let rq_pri = processor.rq.current_priority();
    let cur_pri = cur.effective_priority();
    rq_pri > cur_pri || (ticking && rq_pri >= cur_pri)
}

#[thread_local]
static mut CUR_REBALANCE_TIME: Nanoseconds = 0;
const REBALANCE_TIME: Nanoseconds = 1000000000;

pub fn schedule_maybe_rebalance(dt: Nanoseconds) {
    if !current_processor().is_bsp() {
        return;
    }
    unsafe {
        let newval = CUR_REBALANCE_TIME.checked_sub(dt);
        if let Some(newval) = newval {
            CUR_REBALANCE_TIME = newval;
        } else {
            CUR_REBALANCE_TIME = REBALANCE_TIME / 2 + quick_random() as u64 % REBALANCE_TIME;
            balance(get_cpu_topology());
        }
    }
}

/// Why a woken thread does or does not get the cpu promptly.
///
/// `schedtime.md` measures hand-offs stalling 1-10 ms and, having marked preempt on the local wake
/// path to no effect, is left with two candidate explanations it cannot separate: the woken thread
/// loses the priority comparison (so nothing ever wants to preempt for it), or it wins and the mark
/// is repeatedly swallowed by a critical section. These distinguish them.
///
/// A stall is several ticks, and one tick would bound the wait if the priority test passed at the
/// tick -- so `lost_priority` being the bulk of `local` says the pager's `User + 48` boost is not
/// producing what `pager-srv/src/threads.rs` assumes, and the problem was never preemption.
pub mod wakestats {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Same-cpu wakes reaching the preempt decision, and how it went.
    static LOCAL: AtomicU64 = AtomicU64::new(0);
    static MARKED: AtomicU64 = AtomicU64::new(0);
    static LOST_PRIORITY: AtomicU64 = AtomicU64::new(0);
    static CUR_IDLE: AtomicU64 = AtomicU64::new(0);
    /// Remote wakes, split by whether they actually sent the IPI.
    static REMOTE: AtomicU64 = AtomicU64::new(0);
    static REMOTE_SIGNALLED: AtomicU64 = AtomicU64::new(0);
    /// `schedule_maybe_preempt` found the flag set: acted, or deferred for a critical thread.
    static PREEMPT_TAKEN: AtomicU64 = AtomicU64::new(0);
    static PREEMPT_DEFERRED: AtomicU64 = AtomicU64::new(0);
    /// `needs_reschedule` declined because the current thread was critical. Against the tick count,
    /// this says whether critical sections span whole ticks.
    static RESCHED_CRITICAL: AtomicU64 = AtomicU64::new(0);
    static RESCHED_ASKED: AtomicU64 = AtomicU64::new(0);

    pub fn local(marked: bool, cur_idle: bool) {
        LOCAL.fetch_add(1, Ordering::Relaxed);
        if cur_idle {
            CUR_IDLE.fetch_add(1, Ordering::Relaxed);
        } else if marked {
            MARKED.fetch_add(1, Ordering::Relaxed);
        } else {
            LOST_PRIORITY.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn remote(signalled: bool) {
        REMOTE.fetch_add(1, Ordering::Relaxed);
        if signalled {
            REMOTE_SIGNALLED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn preempt(taken: bool) {
        if taken {
            PREEMPT_TAKEN.fetch_add(1, Ordering::Relaxed);
        } else {
            PREEMPT_DEFERRED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn resched(critical: bool) {
        RESCHED_ASKED.fetch_add(1, Ordering::Relaxed);
        if critical {
            RESCHED_CRITICAL.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// How a wake was classified, stamped on the thread and read back when it reaches a cpu.
    /// `0` is "no wake outstanding".
    pub const WAKE_LOCAL_MARKED: u32 = 1;
    pub const WAKE_LOCAL_LOST: u32 = 2;
    pub const WAKE_LOCAL_IDLE: u32 = 3;
    pub const WAKE_REMOTE: u32 = 4;
    const NR_KINDS: usize = 5;

    /// Upper bounds in microseconds; the last bucket is everything above. The interesting boundary
    /// is one tick (~1 ms): a wake that waited longer than that was not merely un-preempted, it
    /// missed a tick that would have noticed it.
    const BOUNDS_US: [u64; 5] = [10, 100, 1_000, 10_000, 100_000];
    const NR_BUCKETS: usize = BOUNDS_US.len() + 1;

    static LAT_COUNT: [AtomicU64; NR_KINDS] = [const { AtomicU64::new(0) }; NR_KINDS];
    static LAT_SUM: [AtomicU64; NR_KINDS] = [const { AtomicU64::new(0) }; NR_KINDS];
    static LAT_MAX: [AtomicU64; NR_KINDS] = [const { AtomicU64::new(0) }; NR_KINDS];
    static LAT_BUCKET: [[AtomicU64; NR_BUCKETS]; NR_KINDS] =
        [const { [const { AtomicU64::new(0) }; NR_BUCKETS] }; NR_KINDS];

    /// A thread classified `kind` reached a cpu `ns` after being made runnable.
    pub fn wake_to_run(kind: u32, ns: u64) {
        let k = (kind as usize).min(NR_KINDS - 1);
        LAT_COUNT[k].fetch_add(1, Ordering::Relaxed);
        LAT_SUM[k].fetch_add(ns, Ordering::Relaxed);
        LAT_MAX[k].fetch_max(ns, Ordering::Relaxed);
        let idx = BOUNDS_US
            .iter()
            .position(|b| ns / 1000 <= *b)
            .unwrap_or(BOUNDS_US.len());
        LAT_BUCKET[k][idx].fetch_add(1, Ordering::Relaxed);
    }

    fn print_lat(name: &str, k: usize) {
        let n = LAT_COUNT[k].load(Ordering::Relaxed);
        if n == 0 {
            return;
        }
        logln!(
            "  wake->run {}: n={} mean={}us max={}us [<=10us {} <=100us {} <=1ms {} <=10ms {} \
             <=100ms {} >100ms {}]",
            name,
            n,
            LAT_SUM[k].load(Ordering::Relaxed) / n / 1000,
            LAT_MAX[k].load(Ordering::Relaxed) / 1000,
            LAT_BUCKET[k][0].load(Ordering::Relaxed),
            LAT_BUCKET[k][1].load(Ordering::Relaxed),
            LAT_BUCKET[k][2].load(Ordering::Relaxed),
            LAT_BUCKET[k][3].load(Ordering::Relaxed),
            LAT_BUCKET[k][4].load(Ordering::Relaxed),
            LAT_BUCKET[k][5].load(Ordering::Relaxed),
        );
    }

    pub fn print() {
        let local = LOCAL.load(Ordering::Relaxed);
        if local == 0 && REMOTE.load(Ordering::Relaxed) == 0 {
            return;
        }
        logln!(
            "== wakes: {} local ({} marked preempt, {} lost on priority, {} onto an idle cpu), {} \
             remote ({} signalled) ==",
            local,
            MARKED.load(Ordering::Relaxed),
            LOST_PRIORITY.load(Ordering::Relaxed),
            CUR_IDLE.load(Ordering::Relaxed),
            REMOTE.load(Ordering::Relaxed),
            REMOTE_SIGNALLED.load(Ordering::Relaxed),
        );
        logln!(
            "  preempt marks: {} acted on, {} deferred for a critical thread; needs_reschedule \
             asked {} times, declined {} for critical",
            PREEMPT_TAKEN.load(Ordering::Relaxed),
            PREEMPT_DEFERRED.load(Ordering::Relaxed),
            RESCHED_ASKED.load(Ordering::Relaxed),
            RESCHED_CRITICAL.load(Ordering::Relaxed),
        );
        print_lat("local-marked", WAKE_LOCAL_MARKED as usize);
        print_lat("local-lost-pri", WAKE_LOCAL_LOST as usize);
        print_lat("local-onto-idle", WAKE_LOCAL_IDLE as usize);
        print_lat("remote", WAKE_REMOTE as usize);
    }
}

#[thread_local]
static PREEMPT: AtomicBool = AtomicBool::new(false);
pub fn schedule_mark_preempt() {
    PREEMPT.store(true, Ordering::Release);
}

pub fn schedule_maybe_preempt() {
    if !PREEMPT.load(Ordering::Acquire) {
        return;
    }
    // Left set, not consumed, when we cannot act on it. `schedule` refuses outright for a critical
    // thread, so swapping the flag to false first -- as this did -- threw the preemption away and
    // the woken thread waited for the next tick to be noticed again. Every wake that matters here
    // is marked from inside the waker's critical section (`Request::signal`, `requeue_all`), so
    // that was the common case, not a corner.
    if current_thread_ref().is_some_and(|cur| cur.is_critical()) {
        wakestats::preempt(false);
        return;
    }
    if !PREEMPT.swap(false, Ordering::AcqRel) {
        return;
    }
    wakestats::preempt(true);
    let t = crate::interrupt::profile_now();
    let cp = current_processor();
    cp.stats.preempts.fetch_add(1, Ordering::Relaxed);
    schedule(SchedFlags::PREEMPT | SchedFlags::REINSERT);
    crate::interrupt::record_preempt(t);
}

pub fn schedule_hardtick() -> Option<u64> {
    let cp = current_processor();
    // Relaxed on purpose: a free-running counter with no other memory ordered against it.
    cp.stats.hardticks.fetch_add(1, Ordering::Relaxed);
    let resched = needs_reschedule(true);
    let cur = current_thread_ref()?;
    let (current_tick, diff) = cp.rq.hardtick();
    let cur_pri = cur.effective_priority();
    let ts_expire = cur.sched.pay_ticks(diff, cp.rq.timeslice(cur_pri.class));
    let rq_pri = cp.rq.current_priority();
    if resched || ts_expire {
        log::trace!(
            "preempt {}: {} {} (supplying {} ms, {}), {} {}",
            cur.id(),
            resched,
            ts_expire,
            cp.rq.timeslice(rq_pri.max(cur_pri).class),
            rq_pri >= cur_pri,
            current_tick,
            diff,
        );
        schedule_mark_preempt();
    }
    Some(cp.rq.timeslice(rq_pri.max(cur_pri).class))
}

pub fn schedule_resched() {
    current_processor()
        .stats
        .wakeups
        .fetch_add(1, Ordering::Relaxed);
    let is_idle = current_thread_ref().map_or(true, |t| t.is_idle_thread());
    if is_idle || needs_reschedule(false) {
        schedule_mark_preempt();
    }
}

#[thread_local]
static STAT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Wall-clock statticks. The BSP broadcasts each statclock tick to every CPU, so only the BSP
/// increments this -- otherwise it would advance once per CPU per tick. Unlike the per-CPU
/// STAT_COUNTER, this stays comparable across a thread migration.
static STAT_TICKS: AtomicU64 = AtomicU64::new(0);

/// Current wall-clock stattick count, the time basis for `ThreadStats`.
pub fn current_stat_ticks() -> u64 {
    STAT_TICKS.load(Ordering::SeqCst)
}

const PRINT_STATS: bool = false;
pub fn schedule_stattick(dt: Nanoseconds) {
    schedule_maybe_rebalance(dt);

    let s = STAT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let cp = current_processor();
    if cp.is_bsp() {
        STAT_TICKS.fetch_add(1, Ordering::Relaxed);
    }
    let cur = current_thread_ref();
    if let Some(cur) = cur {
        if !cur.is_critical() && cur.is_in_user() && cur.get_mutex_count() == 0 {
            cp.cleanup_exited();
            // TODO: need to call this much more rarely, and not from within a scheduler tick.
            //TRACE_MGR.process_async_and_maybe_flush();
        }
        if cur.is_idle_thread() {
            cp.stats.idle.fetch_add(1, Ordering::Relaxed);
        } else {
            cp.stats.non_idle.fetch_add(1, Ordering::Relaxed);
            /* Update thread stats */
            if cur.is_in_user() {
                cur.stats.user.fetch_add(1, Ordering::Relaxed);
            } else {
                cur.stats.sys.fetch_add(1, Ordering::Relaxed);
            }

            // Statticks since we last saw this thread running. The current one is already
            // charged to user/sys above; the rest is time it wasn't scheduled. This keeps
            // idle+user+sys equal to elapsed statticks, which is what `top` divides by.
            let now = current_stat_ticks();
            let last = cur.stats.last.swap(now, Ordering::Relaxed);
            cur.stats.idle.fetch_add(
                now.saturating_sub(last).saturating_sub(1),
                Ordering::Relaxed,
            );
        }
    }

    cp.rq.clock();

    if PRINT_STATS && s % 200 == 0 {
        if true {
            logln!(
                "STAT {}; {}({}): load {:2},{:2} (ts = {:3}ms), i {:4}, ni {:4}, sw {:4}, w {:4}, p {:4}, h {:4}, s {:4}",
                cp.id,
                cur.as_ref().unwrap().id(),
                cur.unwrap().is_idle_thread(),
                cp.current_load(),
                cp.rq.current_timeshare_load(),
                cp.rq.timeslice(cp.current_priority().class),
                cp.stats.idle.load(Ordering::SeqCst),
                cp.stats.non_idle.load(Ordering::SeqCst),
                cp.stats.switches.load(Ordering::SeqCst),
                cp.stats.wakeups.load(Ordering::SeqCst),
                cp.stats.preempts.load(Ordering::SeqCst),
                cp.stats.hardticks.load(Ordering::SeqCst),
                cp.stats.steals.load(Ordering::SeqCst),
            );
        }
        if cp.id == 0 {
            let all_threads = ALL_THREADS.lock();
            for t in all_threads.values() {
                if !t.is_idle_thread() && t.get_state() == ExecutionState::Running {
                    logln!(
                        "thread {} on {}: u {:4} s {:4} i {:4}, {:?}, {:x}",
                        t.objid(),
                        t.sched.last_cpu.load(Ordering::SeqCst),
                        t.stats.user.load(Ordering::SeqCst),
                        t.stats.sys.load(Ordering::SeqCst),
                        t.stats.idle.load(Ordering::SeqCst),
                        t.get_state(),
                        t.flags.load(Ordering::SeqCst)
                    );
                }
            }
        }
        //crate::clock::print_info();
    }
}
