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

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
    u64,
};

use bitset_core::BitSet;
use intrusive_collections::{KeyAdapter, RBTree, intrusive_adapter};
use twizzler_abi::{
    object::ObjID,
    thread::ExecutionState,
    trace::{SwitchFlags, ThreadCtxSwitch, ThreadMigrate, TraceEntryFlags, TraceKind},
};

pub const MAX_TIMESLICE_TICKS: u32 = 100;
pub const MIN_TIMESLICE_TICKS: u32 = 2;
pub const DEFAULT_TIMESLICE_TICKS: u32 = 32;
/// How long a thread runs before an equal-priority wake may take the cpu from it, and the
/// tick a cpu arms to enforce it. Measured in time from switch-in: a spinner reinserted by every
/// wake never accumulated a paid tick on a non-bsp cpu (8 ms ticks), so a 1 ms sleeper there
/// always waited for the armed tick while the bsp preempted at once.
const WAKE_GRAN_TICKS: u64 = 1;
const WAKE_GRAN_NS: u64 = 1_000_000;

use super::{
    mp::{current_processor, get_processor},
    rq::RunQueue,
};
use crate::{
    clock::{Nanoseconds, get_current_ticks},
    interrupt,
    once::Once,
    processor::{Processor, mp::MAX_CPU_ID, topology::CacheDesc},
    spinlock::Spinlock,
    thread::{
        Thread, ThreadRef, current_thread_ref,
        priority::{Priority, PriorityClass},
        set_current_thread,
        time::Affinity,
    },
    trace::{
        mgr::{TRACE_MGR, TraceEvent, is_thread_ktrace_thread},
        new_trace_entry_thread,
    },
    utils::quick_random,
};

/// What a node of the topology tree groups. `Core` is always the leaf and holds a core's SMT
/// threads; `Cache` is a grouping that exists only because a cache is shared at it.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum CPUTopoType {
    System,
    Package,
    Die,
    Module,
    Cluster,
    Cache,
    Core,
    Other,
}

// A userspace `CpuMask` converts to a `CpuSet` word for word.
const _: () = assert!(twizzler_abi::syscall::CPU_MASK_WORDS == MAX_CPU_ID / 64);

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

    pub fn from_words(set: [u64; MAX_CPU_ID / 64]) -> Self {
        Self { set }
    }

    pub fn words(&self) -> &[u64; MAX_CPU_ID / 64] {
        &self.set
    }

    pub fn count(&self) -> usize {
        self.set.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The lowest cpu in the set.
    pub fn first(&self) -> Option<u32> {
        self.set
            .iter()
            .enumerate()
            .find(|(_, w)| **w != 0)
            .map(|(i, w)| i as u32 * 64 + w.trailing_zeros())
    }
}

/// Numbers nodes and cache instances as they are built, so userspace can tell "same node" and
/// "same cache" apart across cpus without seeing the tree.
static NEXT_NODE_ID: AtomicU32 = AtomicU32::new(0);
static NEXT_CACHE_ID: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
pub struct CPUTopoNode {
    level_type: CPUTopoType,
    id: u32,
    count: usize,
    cpuset: CpuSet,
    first: u32,
    last: u32,
    children: Vec<CPUTopoNode>,
    parent: AtomicPtr<CPUTopoNode>,
    /// Caches shared by exactly this node's cpus, with their instance ids.
    caches: Vec<(u32, CacheDesc)>,
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
            id: NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed),
            count: 0,
            caches: alloc::vec![],
        }
    }

    pub fn kind(&self) -> CPUTopoType {
        self.level_type
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn children(&self) -> &[CPUTopoNode] {
        &self.children
    }

    pub fn caches(&self) -> &[(u32, CacheDesc)] {
        &self.caches
    }

    pub fn add_cache(&mut self, cache: CacheDesc) {
        self.caches
            .push((NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed), cache));
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

/// Both answers [select_cpu] needs, from one pass: the least-loaded cpu that passes the priority
/// filter (`filtered`), and the least-loaded cpu overall (`global`). These used to be two full
/// walks -- each reading every cpu's priority and load lines, remote and rewritten on every switch
/// -- with the second run whenever the first found nothing or found the caller's avoided cpu.
///
/// Returns true (and stops walking) on finding an idle cpu that passes the filter. This is not a
/// policy change: an idle cpu's jittered load is exactly 0 (`(0 * 256).saturating_sub(j)`, and a
/// busy cpu's is at least 129), the comparison is strict `<`, so the first zero visited wins the
/// complete walk too.
///
/// `exclude` is a subtree already searched by the caller's previous, narrower call, so an
/// outward walk visits each cpu once; `avoid` is a cpu never to answer with. `now` collects the
/// least loaded cpu that would run the thread *at once* -- idle, or running something it
/// outranks -- which is the only kind of hit that should stop an outward walk: `filtered`
/// admits an equal-priority busy cpu, where the thread would only be queued, and a walk that
/// stopped at its own core on that basis kept a ping-pong pair on one cpu with the idle cpus
/// stealing the partner back 300k times a run.
fn find_cpus_from_topo(
    node: &CPUTopoNode,
    pri: Option<&Priority>,
    allowed: &Affinity,
    exclude: Option<&CPUTopoNode>,
    avoid: Option<u32>,
    now: &mut Option<SearchCPUResult>,
    filtered: &mut Option<SearchCPUResult>,
    global: &mut Option<SearchCPUResult>,
) -> bool {
    if !node.children.is_empty() {
        for child in &node.children {
            if exclude.is_some_and(|ex| core::ptr::eq(child, ex)) {
                continue;
            }
            if find_cpus_from_topo(child, pri, allowed, exclude, avoid, now, filtered, global) {
                return true;
            }
        }
        return false;
    }
    for c in node.first..=node.last {
        if !node.cpuset.contains(c) || !allowed.allows(c) || avoid == Some(c) {
            continue;
        }
        let processor = get_processor(c);
        let load = processor.current_load();
        /* jitter. This is similar to how freebsd does things */
        let mut jload = (load * 256).saturating_sub((quick_random() % 128) as u64);
        // An idle hyperthread of a busy core shares that core's pipeline: worth less than an
        // idle core (0) and more than any busy cpu (at least 129), and not a perfect hit.
        let sibling_busy = load == 0
            && !crate::flat_placement()
            && node.count > 1
            && (node.first..=node.last)
                .any(|s| s != c && node.cpuset.contains(s) && !get_processor(s).is_idle());
        if sibling_busy {
            jload = 128;
        }
        if global.as_ref().is_none_or(|g| jload < g.load) {
            *global = Some(SearchCPUResult {
                load: jload,
                cpuid: c,
            });
        }
        let current = processor.current_priority();
        if pri.is_none_or(|pri| &current <= pri) {
            if filtered.as_ref().is_none_or(|f| jload < f.load) {
                *filtered = Some(SearchCPUResult {
                    load: jload,
                    cpuid: c,
                });
            }
            if (load == 0 || pri.is_some_and(|pri| pri > &current))
                && now.as_ref().is_none_or(|n| jload < n.load)
            {
                *now = Some(SearchCPUResult {
                    load: jload,
                    cpuid: c,
                });
            }
            if load == 0 && !sibling_busy {
                return true;
            }
        }
    }
    false
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

/// Stamped when a thread leaves a cpu (reinsertion, or the switch-out of a thread that
/// blocked) and at creation; `RunQueue::insert` compares it. Stamping at switch-in made a
/// thread that ran out its slice "past its deadline" at the reinsertion, so it was filed in the
/// realtime queue and took the cpu straight back from whoever was queued (schedtest: 6
/// spinners on 4 cpus, a queued thread starved for the whole second).
fn set_deadline(thread: &Thread, processor: &Processor) {
    thread.sched.set_deadline(
        get_current_ticks() + processor.rq.deadline(thread.effective_priority().class),
    );
}

fn schedule_thread_on_cpu(thread: ThreadRef, processor: &Processor, is_new: bool, is_wake: bool) {
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
    // 3-4 stalls that make every whole-boot mean; this is per wake.
    //
    // A reinsertion is not a wake, and excluding it is load-bearing twice over. `do_schedule`
    // routes the *current* thread back through here with `is_current = false` on the REINSERT
    // path, so without this check (a) every preemption-driven reinsertion was stamped and
    // counted as a wake, which polluted the histogram, and (b) under the `>=` below a
    // reinserted thread compares equal to *itself* and marks preempt, so each preemption
    // produces another. That self-sustaining loop is what the first `>=` attempt actually
    // measured -- 6820 marks against 1129, info pickup 343-467 -> 657-713 us -- rather than the
    // equal-priority thrash it was blamed on.
    // Resolved once for both uses below: each call is an Arc clone/drop pair.
    let cur = current_thread_ref();
    let is_reinsertion = cur.as_ref().is_some_and(|cur| cur.id() == thread.id());
    if is_reinsertion {
        set_deadline(&thread, processor);
    }
    // A thread the insert below files in the realtime queue -- past its deadline, i.e. it has
    // waited its fair share -- preempts a timeshare thread the way a higher priority does.
    let boosted = !is_reinsertion && processor.rq.files_realtime(&thread);
    // A timeshare *wake* (not a migration or reinsertion) goes to the front of the calendar and
    // rotates at equal priority: at once if the running thread has had its `WAKE_GRAN_TICKS`,
    // else at that tick, which the wake path arms (`needs_reschedule`).
    let timeshare_wake = is_wake
        && !is_reinsertion
        && !boosted
        && woken_priority.class == PriorityClass::User;
    let kind = if is_reinsertion {
        0
    } else if is_remote {
        wakestats::WAKE_REMOTE
    } else {
        match cur.as_ref() {
            Some(cur) if cur.is_idle_thread() => wakestats::WAKE_LOCAL_IDLE,
            Some(cur) if boosted || woken_priority > cur.effective_priority() => {
                // A thread holding a mutex is in a critical section that some waiter -- possibly
                // the one being woken -- is queued behind, so stopping it there trades a short
                // hold for a scheduling round trip plus a wake for everyone waiting. Deferred only
                // within a class: a higher *class* preempts a holder as it always did, so realtime
                // is never held off by a user thread.
                if cur.get_mutex_count() > 0
                    && woken_priority.class <= cur.effective_priority().class
                {
                    wakestats::holder_spared();
                    wakestats::WAKE_LOCAL_LOST
                } else {
                    wakestats::WAKE_LOCAL_MARKED
                }
            }
            Some(cur) if timeshare_wake && woken_priority == cur.effective_priority() => {
                if cur.sched.ran_ns(crate::instant::current_ns()) < WAKE_GRAN_NS {
                    wakestats::WAKE_LOCAL_LOST
                } else if cur.get_mutex_count() > 0 {
                    wakestats::holder_spared();
                    wakestats::WAKE_LOCAL_LOST
                } else {
                    wakestats::WAKE_LOCAL_MARKED
                }
            }
            Some(_) => wakestats::WAKE_LOCAL_LOST,
            None => 0,
        }
    };
    // A *separate* label for the latency histogram only. `kind` drives the preempt decision in
    // the `match` below, whose `_ => {}` arm would silently swallow any new variant -- a local
    // new-thread wake would stop calling `schedule_mark_preempt`, which is a scheduling
    // regression wearing an instrument's clothes. So the behaviour classification is left exactly
    // as it was and only the histogram bucket is refined.
    //
    // Splits spawn out of `remote`/`local-*`: the spawn path's wake is the one this measures
    // (`schedule_new_thread` -> the child's first instruction), and it was previously averaged in
    // with every futex and queue wake in the system, where ~130k samples a boot bury the ~3k that
    // a spawn campaign cares about.
    let lat_kind = if kind != 0 && is_new {
        if is_remote {
            wakestats::WAKE_NEW_REMOTE
        } else {
            wakestats::WAKE_NEW_LOCAL
        }
    } else {
        kind
    };
    if kind != 0 && crate::kdiag_wake() {
        thread.sched.wake_ticks.store(
            crate::instant::Instant::now().raw_ticks().max(1),
            Ordering::Relaxed,
        );
        thread.sched.wake_kind.store(lat_kind, Ordering::Relaxed);
    }

    thread.sched.moving_to_queue(processor.id);
    thread.sched.reset_timeslice();
    processor.rq.insert(thread, timeshare_wake);
    // The bsp ticks every ms regardless; a remote target arms from `schedule_resched`.
    if timeshare_wake && kind != wakestats::WAKE_LOCAL_MARKED && !is_remote && !processor.is_bsp() {
        crate::clock::schedule_oneshot_tick(WAKE_GRAN_TICKS);
    }

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
    // At smp1 that is *every* wake in the system, which is why measurements put the pager's
    // lane pickup at 372-456 us there while the same hop costs 25-36 us at smp4.
    //
    // Marked rather than switched: this runs inside the waker's critical section on most paths
    // (`Request::signal`, `requeue_all`), where switching is forbidden. The flag is consumed at the
    // next interrupt return, which `schedule_maybe_preempt` now defers if we are still critical.
    //
    // A reinsertion is excluded above: that is `schedule` requeueing the thread it is already
    // running, not a wake, and marking preempt for it would ask the scheduler to preempt in
    // favour of itself.
    match kind {
        // Waking anything while this cpu is *idling* must preempt, and there is no priority
        // question to ask: the idle thread has no work and nothing to protect.
        // `schedule_resched` -- the ipi handler -- already says exactly this (`if is_idle
        // || needs_reschedule()`), but no local wake reached it, so an idling cpu sat
        // until the next tick with a runnable thread beside it. 367-370 wakes a boot at
        // smp1, measured at ~400 us mean with a 144-146 ms outlier every run:
        // the worst latencies measured anywhere, and the only class where the delay
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
    let th = processor.rq.take(new_cpu_rq != processor.id)?;
    // The queue hands out its head without regard to affinity (a pinned thread only keeps the
    // queue's `movable` count from reaching zero). One that may not run on the target goes back
    // through placement instead of over.
    let th = admit_on(th, new_cpu_rq)?;
    th.sched.moving_to_queue(new_cpu_rq);
    Some(th)
}

/// `th` was taken off a queue for `cpu`. If its affinity excludes `cpu`, queue it somewhere it
/// may run and report nothing taken.
fn admit_on(th: ThreadRef, cpu: u32) -> Option<ThreadRef> {
    if th.sched.affinity.allows(cpu) {
        return Some(th);
    }
    let cpuid = select_cpu(&th, Some(cpu));
    schedule_thread_on_cpu(th, get_processor(cpuid), false, false);
    None
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
            return us.rq.take(false).and_then(|th| admit_on(th, us.id));
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
    let mut steps = 0;
    balance_node(topo, &mut steps);
}

const MAX_STEPS: usize = 20;

/// Children first, so load evens out inside each cache domain before anything crosses one; a
/// move between this node's cpus then only answers an imbalance its domains could not fix
/// among themselves.
fn balance_node(node: &CPUTopoNode, steps: &mut usize) {
    if !crate::flat_placement() {
        for child in &node.children {
            if child.count > 1 {
                balance_node(child, steps);
            }
        }
    }
    if node.count < 2 {
        return;
    }
    let mut allowed_set = node.cpuset;
    while *steps < MAX_STEPS {
        let Some(result) = choose_cpu_balance(node, &allowed_set) else {
            break;
        };
        {
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

            if donor.rq.current_load() > 0 {
                // Only a donor with something queued asks its running thread to move too. With
                // nothing queued the load is that one thread, and marking it sent every lone busy
                // thread hopping to an idle cpu on each pass, for no load gained.
                donor.set_rebalance();
                allowed_set.remove(result.recipient);
                let thread = take_a_thread_from_cpu(donor, recipient.id);
                if let Some(thread) = thread {
                    log::trace!(
                        "rebalanced {} ({} -> {})",
                        thread.id(),
                        donor.id,
                        recipient.id
                    );
                    schedule_thread_on_cpu(thread, recipient, false, false);
                    *steps += 10;
                }
            } else if donor.current_load() == 1 {
                allowed_set.remove(result.donor);
            }
        }
        *steps += 1;
    }
}

fn select_cpu(thread: &ThreadRef, try_avoid: Option<u32>) -> u32 {
    /* TODO: take SMT into acount */
    let affinity = &thread.sched.affinity;
    let stats = &current_processor().stats;
    /* 0: a thread allowed exactly one cpu has nothing to choose. */
    if let Some(cpu) = thread.sched.pinned_to() {
        stats.pick_pinned.fetch_add(1, Ordering::Relaxed);
        return cpu;
    }
    let pri = thread.effective_priority();
    let last = thread
        .sched
        .preferred_cpu()
        .map(|(cpu, _pinned)| cpu)
        .filter(|cpu| affinity.allows(*cpu) && try_avoid.is_none_or(|ta| ta != *cpu));
    let flat = crate::flat_placement();
    let warm = flat || thread.sched.is_warm();
    /* 1: the last cpu, while the thread's data is still in its caches and it can run there at
     * once. Idle, not "nothing queued": a cpu running one thread has an empty queue too, and
     * sending every wake back there piled five spinners on one cpu beside three idle ones. */
    if let Some(last) = last.filter(|_| warm) {
        let processor = get_processor(last);
        if processor.is_idle() || pri > processor.current_priority() {
            stats.pick_last_warm.fetch_add(1, Ordering::Relaxed);
            return last;
        }
    }

    /* 2: outward from the last cpu's core, each level of the tree one more shared cache away:
     * the first level holding a cpu that will run the thread at once wins, and a tie in load
     * goes to the cpu seen first, which is the nearer one. Cold, or never run, the whole
     * tree. */
    let topo = get_cpu_topology();
    let mut now = None;
    let mut filtered = None;
    let mut global = None;
    let mut node = Some(
        last.filter(|_| warm && !flat)
            .and_then(|last| topo.find_cpu(last))
            .unwrap_or(topo),
    );
    let mut searched = None;
    while let Some(n) = node {
        find_cpus_from_topo(
            n,
            Some(&pri),
            affinity,
            searched,
            try_avoid,
            &mut now,
            &mut filtered,
            &mut global,
        );
        if let Some(res) = &now {
            if searched.is_none() {
                stats.pick_near.fetch_add(1, Ordering::Relaxed);
            } else {
                stats.pick_far.fetch_add(1, Ordering::Relaxed);
            }
            if last.is_some_and(|l| l != res.cpuid) {
                stats.pick_migrate.fetch_add(1, Ordering::Relaxed);
            }
            return res.cpuid;
        }
        searched = Some(n);
        node = n.parent();
    }
    /* 3: nothing runs it at once anywhere: the least loaded cpu it would at least queue on at
     * its own priority, else the least loaded of all. */
    if let Some(res) = filtered.or(global) {
        // Nothing will run it at once. Cold or not, the last cpu keeps it if it is no busier
        // than the pick: whatever survives in its caches is free, a migration is not.
        if let Some(last) = last.filter(|_| !flat) {
            if get_processor(last).current_load() <= res.load / 256 {
                stats.pick_last_cold.fetch_add(1, Ordering::Relaxed);
                return last;
            }
            stats.pick_migrate.fetch_add(1, Ordering::Relaxed);
        }
        stats.pick_lowest.fetch_add(1, Ordering::Relaxed);
        return res.cpuid;
    }

    // `SetAffinity` refuses a mask naming no cpu that is up, so this is a mask installed before
    // the tree was built. Anywhere it names, else anywhere at all.
    let mut fallback = None;
    crate::processor::mp::with_each_active_processor(|p| {
        if fallback.is_none() && affinity.allows(p.id) {
            fallback = Some(p.id);
        }
    });
    stats.pick_fallback.fetch_add(1, Ordering::Relaxed);
    fallback.unwrap_or_else(|| current_processor().id)
}

intrusive_adapter!(pub AllThreadsAdapter = ThreadRef: Thread { all_threads_link: intrusive_collections::rbtree::AtomicLink });
impl<'a> KeyAdapter<'a> for AllThreadsAdapter {
    type Key = u64;
    fn get_key(&self, t: &'a Thread) -> u64 {
        t.id()
    }
}

intrusive_adapter!(pub AllThreadsReprAdapter = ThreadRef: Thread { all_threads_repr_link: intrusive_collections::rbtree::AtomicLink });
impl<'a> KeyAdapter<'a> for AllThreadsReprAdapter {
    type Key = ObjID;
    fn get_key(&self, t: &'a Thread) -> ObjID {
        t.objid()
    }
}

/// Every live thread, by [`Thread::id`]. Intrusive rather than a `BTreeMap` so that spawn and exit
/// neither allocate nor free under this spinlock -- the kernel allocator can re-enter the frame
/// allocator, and this lock is taken with interrupts off.
static ALL_THREADS: Spinlock<RBTree<AllThreadsAdapter>> =
    Spinlock::new(RBTree::new(AllThreadsAdapter::NEW));
/// The same threads, by control-object id, for [`lookup_thread_repr`].
static ALL_THREADS_REPR: Spinlock<RBTree<AllThreadsReprAdapter>> =
    Spinlock::new(RBTree::new(AllThreadsReprAdapter::NEW));

/// Run `f` over every live thread.
///
/// **`f` runs inside a spinlock, so it must not take a mutex, sleep, or fault.** Anything that
/// ends in `Mutex::lock` panics with "cannot lock mutex in critical context" -- including the
/// non-obvious route `lookup_object(..) -> get_notes().summarize(..)`, which reaches
/// `ControlObjectCacher`'s notes mutex. That is not a hypothetical: the hang-report table did
/// exactly this and killed 10/10 sysbench boots on 2026-08-27, printing its header and no rows.
///
/// If you need anything mutex-guarded per thread, snapshot first and do the work outside:
///
/// ```ignore
/// let mut threads: heapless::Vec<ThreadRef, N> = heapless::Vec::new();
/// with_all_threads(|at| {
///     let mut cursor = at.front();
///     while let Some(t) = cursor.clone_pointer() {
///         cursor.move_next();
///         let _ = threads.push(t);
///     }
/// });
/// for t in threads.iter() { /* mutexes are safe here */ }
/// ```
///
/// `clone_pointer` rather than `iter()`: the iterator yields borrows that cannot outlive the
/// guard, which is the whole point.
pub fn with_all_threads<F>(mut f: F)
where
    F: FnMut(&RBTree<AllThreadsAdapter>),
{
    let guard = ALL_THREADS.lock();
    f(&guard);
}

pub fn with_each_thread<F>(mut f: F)
where
    F: FnMut(&Thread),
{
    let guard = ALL_THREADS.lock();
    for th in guard.iter() {
        f(th);
    }
}

/// One compact snapshot of scheduler state, for the test-mode schedmon thread (`main.rs`).
///
/// The idle-loop hang diagnostics are structurally blind at smp1 whenever a USER thread spins --
/// the bsp never idles -- which is exactly the state the release-smp1 wedge leaves the system in.
/// This runs from a REALTIME thread instead, so a spinner cannot starve it. Two passes 30s apart
/// separate the wedge shapes: a spinner shows `run true` with a fresh `ip` and advancing cpu
/// counters; a stranded runnable thread shows `Running, run false` with a linked or unlinked
/// sched_link and frozen ticks; a lost wake shows `Sleeping` plus whichever wait link it holds.
pub fn schedmon_dump(pass: u64) {
    for p in crate::processor::mp::all_processors().iter().flatten() {
        // Host steal time, so a reading taken on a contended host says so in the same line as
        // the numbers it perturbs. 0 outside KVM (and always on non-x86).
        #[cfg(target_arch = "x86_64")]
        let steal_ms = crate::arch::kvm::steal_time_ns(p) / 1_000_000;
        #[cfg(not(target_arch = "x86_64"))]
        let steal_ms = 0u64;
        emerglogln!(
            // st i/ni: statclock samples (idle/non-idle). Their combined rate against the 30s
            // dump interval is the check that each cpu's statclock is actually ticking at its
            // configured frequency, now that it is per-cpu rather than a bsp broadcast.
            "[schedmon] {} cpu {}: ht {} sw {} pre {} wake {} load {} ts_load {} rq_pri {:?} requeue {} steal_ms {} st {}/{}",
            pass,
            p.id,
            p.stats.hardticks.load(Ordering::Relaxed),
            p.stats.switches.load(Ordering::Relaxed),
            p.stats.preempts.load(Ordering::Relaxed),
            p.stats.wakeups.load(Ordering::Relaxed),
            p.rq.current_load(),
            p.rq.current_timeshare_load(),
            p.rq.current_priority(),
            crate::syscall::sync::requeue_len(),
            steal_ms,
            p.stats.idle.load(Ordering::Relaxed),
            p.stats.non_idle.load(Ordering::Relaxed),
        );
    }
    let reprio = reprioritized_counts();
    emerglogln!(
        "[schedmon] {} reprio rt {} ts {} idle {}",
        pass,
        reprio[0],
        reprio[1],
        reprio[2]
    );
    with_all_threads(|at| {
        for t in at.iter() {
            if t.is_idle_thread() || t.get_state() == ExecutionState::Exited {
                continue;
            }
            emerglogln!(
                "[schedmon] {}   th {} ({}) {:?} pri {:?} run {} crit {} lnk sc{} rq{} sy{} mx{} cv{} mw{} pg{} tw{} u {} s {} ip {:x}",
                pass,
                t.id(),
                t.objid(),
                t.get_state(),
                t.effective_priority(),
                t.is_active_running(),
                t.is_critical(),
                t.sched_link.is_linked() as u8,
                t.requeue_link.is_linked() as u8,
                t.sync_links.is_linked() as u8,
                t.mutex_link.is_linked() as u8,
                t.condvar_link.is_linked() as u8,
                t.memwait_link.is_linked() as u8,
                t.pager_link.is_linked() as u8,
                t.has_timed_wait() as u8,
                t.stats.user.load(Ordering::Relaxed),
                t.stats.sys.load(Ordering::Relaxed),
                t.read_ip(),
            );
        }
    });
}

/// Take a thread out of both registries.
///
/// The refs are dropped *after* their guards, not inside them. `Thread::drop` reaches
/// `IdCounter::release`'s sleeping mutex -- not from its `Drop` body, which is lock-free and a
/// spinlock, but from the implicit drop of its `id: Id<'static>` field -- and both locks here are
/// spinlocks held with interrupts off. That is currently unreachable: the sole caller is `exit()`,
/// where the exiting thread is `current_thread_ref` and so is a live local, and `self_reference`
/// holds a second ref that is reclaimed only later, in the reap path. Dropping outside the guards
/// means a future caller that is *not* the exiting thread does not turn that into a wedge.
pub fn remove_thread(id: u64) {
    let t = ALL_THREADS.lock().find_mut(&id).remove();
    let Some(t) = t else {
        return;
    };
    let repr = ALL_THREADS_REPR.lock().find_mut(&t.objid()).remove();
    drop(repr);
    drop(t);
}

pub fn lookup_thread_repr(id: ObjID) -> Option<ThreadRef> {
    ALL_THREADS_REPR.lock().find(&id).clone_pointer()
}

pub fn schedule_new_thread(thread: Thread) -> ThreadRef {
    wakestats::new_thread();
    thread.set_state(ExecutionState::Running);
    let thread = Arc::new(thread);
    {
        ALL_THREADS.lock().insert(thread.clone());
        ALL_THREADS_REPR.lock().insert(thread.clone());
    }
    *unsafe { thread.self_reference.get().as_mut().unwrap() } =
        Box::into_raw(Box::new(thread.clone()));
    let cpuid = select_cpu(&thread, None);
    let processor = get_processor(cpuid);
    // A thread that has never run has no wait to be owed for. Without a stamp its zero
    // deadline reads as expired and every spawn jumps the calendar, which ran the child ahead
    // of its spawner's bookkeeping often enough to turn signal-test's "reader sees itself as
    // objid 0x0" race from 0/300 runs into 8/80.
    set_deadline(&thread, processor);
    schedule_thread_on_cpu(thread.clone(), processor, true, false);
    thread
}

/// Re-file a queued runnable thread after its effective priority rose (donation, or a raise from
/// another thread). The run queues bucket by insert-time priority and `take` scans classes
/// strictly in order, so a poke alone (`maybe_reschedule_thread`) cannot help a thread whose
/// donation crossed classes -- it stays in the lower class's structure, unreachable while any
/// higher class has work. Removing it and re-inserting through the ordinary path files it where
/// its new priority says, with all wake/preempt signalling included.
///
/// Successful re-files by source structure, printed in the schedmon header and announced once
/// per source per boot: a fix that compiles and passes its bookkeeping tests but never fires on
/// the live donation path would otherwise be validated only by an absence of wedges -- and a
/// presence that never includes `Idle` as the source would leave the actual starvation case
/// (a Background owner lifted out of the queue `take` cannot reach past a spinner) covered by
/// absence alone.
static REPRIO_COUNTS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static REPRIO_ANNOUNCED: [AtomicBool; 3] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

pub fn reprioritized_counts() -> [u64; 3] {
    [
        REPRIO_COUNTS[0].load(Ordering::Relaxed),
        REPRIO_COUNTS[1].load(Ordering::Relaxed),
        REPRIO_COUNTS[2].load(Ordering::Relaxed),
    ]
}

/// Returns false when the thread was not found queued (it is running, sleeping, or was
/// concurrently taken) -- the caller falls back to the poke.
pub fn reprioritize_queued_thread(thread: &Thread) -> bool {
    // An exiting thread must not be handed to `schedule_thread`, whose early-return would drop
    // what may be the last reference under whatever spinlock our caller holds.
    if thread.is_exiting() {
        return false;
    }
    let Some(cpu) = thread.sched.current_cpu_rq() else {
        return false;
    };
    let Some((th, from)) = get_processor(cpu).rq.remove_thread(thread) else {
        return false;
    };
    let idx = from as usize;
    REPRIO_COUNTS[idx].fetch_add(1, Ordering::Relaxed);
    // Once per boot *per source*, so a sweep's transcripts show not only that the path fires but
    // which structure it lifted from -- `from Idle` is the wedge's own rescue. A print from this
    // context (caller may hold a mutex's queue spinlock) nests the console lock under it, which
    // is one-way and already done by warn-paths elsewhere; thrice per boot bounds the exposure.
    if !REPRIO_ANNOUNCED[idx].swap(true, Ordering::Relaxed) {
        log::debug!(
            "[sched] re-filed queued thread {} from {:?} at {:?}",
            th.id(),
            from,
            th.effective_priority()
        );
    }
    schedule_thread(th);
    true
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
    // After the idle check: an idle thread is not woken in any sense a reader cares about, and
    // counting it would put a wake on every cpu every time one went idle and came back.
    thread.stats.wakes.fetch_add(1, Ordering::Relaxed);
    wakesrc::note(core::panic::Location::caller());
    // `ProcessorStats::wakeups` was declared and read (`InfoKind::KernelStats`, the mutex-stall
    // dump) but never incremented, so it reported zero for every boot. Charged to the waker's
    // cpu, which is the cpu doing the work.
    current_processor()
        .stats
        .wakeups
        .fetch_add(1, Ordering::Relaxed);
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
    schedule_thread_on_cpu(thread, processor, false, true);
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
    let now_ns = crate::instant::current_ns();
    // The outgoing thread owns this cpu's llc misses up to here.
    crate::thread::cachemiss::charge_switch_out(old, now_ns);
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
    thread.sched.stamp_switch_in(now_ns);
    if old.id() != thread.id() {
        trace_switch(&old, &thread, flags);
    }
    cp.stats.switches.fetch_add(1, Ordering::Relaxed);
    thread.stats.switches.fetch_add(1, Ordering::Relaxed);

    if let Some(oldcpu) = oldcpu {
        if oldcpu != cp.id {
            thread.stats.migrations.fetch_add(1, Ordering::Relaxed);
            log::trace!("migrated {} {} -> {}", thread.id(), oldcpu, cp.id);
            trace_migrate(&thread, oldcpu as u64, cp.id as u64);
        }
    }

    if !old.is_idle_thread() {
        set_deadline(old, cp);
        old.sched.left_tick.store(
            crate::instant::Instant::now().raw_ticks().max(1),
            Ordering::Relaxed,
        );
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
    // Why `cur` is leaving, charged only if a switch actually happens below.
    let reason = if cur.is_exiting() {
        &processor.stats.switch_exit
    } else if cur.is_idle_thread() {
        &processor.stats.switch_from_idle
    } else if !flags.contains(SchedFlags::REINSERT) {
        &processor.stats.switch_block
    } else if flags.contains(SchedFlags::PREEMPT) {
        &processor.stats.switch_preempt
    } else {
        &processor.stats.switch_yield
    };

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
        let disallowed_here = !cur.sched.affinity.allows(processor.id);
        if flags.contains(SchedFlags::PREEMPT)
            || processor.must_rebalance()
            || disallowed_here
            || rq_has_higher(cur, &processor.rq, flags.contains(SchedFlags::YIELD))
        {
            let cpuid = if processor.must_rebalance() || disallowed_here {
                select_cpu(&cur, Some(processor.id))
            } else {
                processor.id
            };
            let processor = get_processor(cpuid);
            schedule_thread_on_cpu(cur.clone(), processor, false, false);
        } else {
            // This is a current thread to reinsert, but only count it as such if it is not
            // yielding so that other threads will run first. A yield with nothing queued would
            // only insert and take itself straight back, so it takes the shortcut.
            if flags.contains(SchedFlags::YIELD) && !processor.rq.is_empty() {
                schedule_thread_on_cpu(cur.clone(), processor, false, false);
            } else {
                // shortcut -- we are intending to just run this thread again.
                processor.stats.resched_noop.fetch_add(1, Ordering::Relaxed);
                cur.sched.reset_timeslice();
                processor.exit_idle();
                return;
            }
        }
    }

    while let Some(next) = processor.rq.take(false) {
        // Queued here before its affinity excluded this cpu: send it on and pick again.
        let Some(next) = admit_on(next, processor.id) else {
            continue;
        };
        if &next == cur {
            // We took ourselves back off the queue, so we never reach switch_to (the only other
            // caller of moving_to_active). Clear current_processor_queue here, or we stay marked
            // as queued while actually running.
            cur.sched.moving_to_active(processor.id);
            processor.stats.resched_noop.fetch_add(1, Ordering::Relaxed);
            return;
        }
        reason.fetch_add(1, Ordering::Relaxed);
        switch_to(next, cur, flags);
        return;
    }

    // No thread was found in our queue. Try to steal from another queue.
    if let Some(stolen) = try_steal() {
        let cp = current_processor();
        cp.stats.steals.fetch_add(1, Ordering::SeqCst);
        reason.fetch_add(1, Ordering::Relaxed);
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
    reason.fetch_add(1, Ordering::Relaxed);
    processor
        .stats
        .switch_to_idle
        .fetch_add(1, Ordering::Relaxed);
    switch_to(processor.idle_thread.wait().clone(), cur, flags);
}

pub fn schedule(flags: SchedFlags) {
    let cur = current_thread_ref().unwrap();
    /* TODO: if we preempt, just put the thread back on our list (or decide to not resched) */
    let istate = interrupt::disable();
    if cur.is_critical() {
        // A voluntary block that cannot block. `finish_blocking` has already dropped its
        // CriticalGuard and set the state to Sleeping, so returning here means the caller resumes
        // believing it slept: harmless when it re-checks its condition in a loop, a busy-wait when
        // it does not, and on one cpu a wedge if what it waits for needs this cpu. Naming the site
        // that took the count off zero is the only thing that identifies the offending lock --
        // without it the failure mode suppresses its own evidence. Bounded by the report budget.
        if flags.contains(SchedFlags::YIELD)
            && crate::thread::locktrack::diag::BLOCK_WHILE_CRITICAL.hit()
        {
            let count = cur.critical_counter.load(Ordering::SeqCst);
            match cur.critical_origin() {
                Some(loc) => emerglogln!(
                    "locktrack: thread {} blocked voluntarily while critical (count {}), taken off zero at {}",
                    cur.id(),
                    count,
                    loc,
                ),
                None => emerglogln!(
                    "locktrack: thread {} blocked voluntarily while critical (count {}), origin unknown",
                    cur.id(),
                    count,
                ),
            }
        }
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
    // Its affinity changed under it; `do_schedule`'s reinsert path moves it.
    if !cur.sched.affinity.allows(processor.id) {
        return true;
    }
    if processor.rq.is_empty() {
        return false;
    }
    let cur_pri = cur.effective_priority();
    // The realtime queue -- realtime threads and timeshare ones past their deadline -- runs
    // ahead of every timeshare thread, at once.
    if processor.rq.has_realtime() && cur_pri.class < PriorityClass::Realtime {
        return true;
    }
    // Equal priority rotates for a queued wake once the running thread has had `WAKE_GRAN_TICKS`,
    // and otherwise only on slice expiry (`schedule_hardtick`). Rotating at every tick switched
    // two busy threads on the bsp every ms; rotating on expiry alone made a spin-waiting pair on
    // one cpu wait a slice per hand-off, which a spinner has to avoid by yielding.
    let rq_pri = processor.rq.current_priority();
    rq_pri > cur_pri
        || (ticking
            && rq_pri == cur_pri
            && processor.rq.wake_pending()
            && cur.sched.ran_ns(crate::instant::current_ns()) >= WAKE_GRAN_NS)
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
/// The wake-latency investigation measured hand-offs stalling 1-10 ms and, having marked preempt on
/// the local wake path to no effect, is left with two candidate explanations it cannot separate:
/// the woken thread loses the priority comparison (so nothing ever wants to preempt for it), or it
/// wins and the mark is repeatedly swallowed by a critical section. These distinguish them.
///
/// A stall is several ticks, and one tick would bound the wait if the priority test passed at the
/// tick -- so `lost_priority` being the bulk of `local` says the pager's `User + 48` boost is not
/// producing what `pager-srv/src/threads.rs` assumes, and the problem was never preemption.
/// Wakes attributed to the call site that asked for them.
///
/// `KernelStats::thread_wakes` says how many wakes a workload costs; this says who asked. The
/// site is exact and needs no context tracking: `schedule_thread` is already `#[track_caller]`,
/// so `Location::caller()` names the caller directly.
///
/// An earlier version of this tagged the cpu with the syscall or interrupt vector in flight and
/// read that tag here. It was wrong: `post_interrupt` runs `schedule_maybe_preempt`, which can
/// switch threads *inside* the handler, leaving the incoming thread to run with the cpu still
/// tagged as the interrupt. It charged 56% of all wakes to the reschedule IPI -- a handler that
/// only sets a flag and wakes nothing.
/// Every kernel thread, with what it has cost.
///
/// The sampling profiler groups its kernel-thread samples into one `<kernel thread, no entry
/// frame>` bucket -- 25% of kernel time in the profile that prompted this -- and names the rows it
/// can from a note on the control object. Two of the four could not be named that way, so the
/// largest single bucket had two unidentified occupants. The kernel can read the same notes
/// (`get_notes().summarize`) and already accounts per-thread time, so it can answer directly
/// rather than leaving the question to a join the tracer cannot make.
pub fn print_kernel_threads() {
    // Two passes, and the split is load-bearing: `with_all_threads` holds a spinlock, and
    // `VNotes::summarize` takes a *sleeping* mutex to read the name note. Doing the lookup inside
    // the closure panics outright ("cannot lock mutex in critical context"). So the locked pass
    // collects ids and counters only, and the names are resolved after it has been dropped.
    let mut rows: alloc::vec::Vec<(u64, u64, u64, u64, u64, u64, twizzler_abi::object::ObjID)> =
        alloc::vec::Vec::new();
    with_all_threads(|threads| {
        for thread in threads.iter() {
            // A kernel thread belongs to no compartment. Idle threads are excluded: they are
            // per-cpu by construction and their time is the machine being idle, not a cost.
            if thread.home_sctx_id().raw() != 0 || thread.is_idle_thread() {
                continue;
            }
            rows.push((
                thread.stats.sys.load(Ordering::Relaxed),
                thread.stats.user.load(Ordering::Relaxed),
                thread.stats.idle.load(Ordering::Relaxed),
                thread.stats.wakes.load(Ordering::Relaxed),
                thread.stats.syscalls.load(Ordering::Relaxed),
                thread.id(),
                thread.objid(),
            ));
        }
    });
    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    logln!("== kernel threads (statticks), by sys ==");
    for (sys, user, idle, wakes, syscalls, id, objid) in rows {
        let mut namebuf = [0u8; 24];
        let namelen = match crate::obj::lookup_object(objid, crate::obj::LookupFlags::empty()) {
            crate::obj::LookupResult::Found(obj) => obj.get_notes().summarize(&mut namebuf),
            _ => 0,
        };
        let name = core::str::from_utf8(&namebuf[..namelen]).unwrap_or("?");
        logln!(
            "==   {:<20} id {:<5} sys {:>8} user {:>8} idle {:>10} wakes {:>8} syscalls {:>7}",
            if name.is_empty() { "<unnamed>" } else { name },
            id,
            sys,
            user,
            idle,
            wakes,
            syscalls,
        );
    }
}

pub mod wakesrc {
    use core::{
        panic::Location,
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    /// One slot per distinct call site. `schedule_thread` has a handful; the last slot collects
    /// any overflow so a new caller shows up as "other" rather than going uncounted.
    const NR: usize = 16;

    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO_U: AtomicUsize = AtomicUsize::new(0);
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU64 = AtomicU64::new(0);
    /// The `&'static Location` itself, as a pointer, is the key: two call sites never share one.
    static SITES: [AtomicUsize; NR] = [ZERO_U; NR];
    static COUNTS: [AtomicU64; NR] = [ZERO; NR];
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    pub fn note(loc: &'static Location<'static>) {
        TOTAL.fetch_add(1, Ordering::Relaxed);
        if !crate::kdiag_wake() {
            return;
        }
        let key = loc as *const _ as *const u8 as usize;
        for i in 0..NR - 1 {
            let cur = SITES[i].load(Ordering::Relaxed);
            if cur == key {
                COUNTS[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
            if cur == 0
                && SITES[i]
                    .compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                COUNTS[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        COUNTS[NR - 1].fetch_add(1, Ordering::Relaxed);
    }

    /// Threads made runnable, summed over every site. Unlike `ProcessorStats::wakeups` this is
    /// only that: `schedule_resched` charges reschedule requests to `wakeups` as well, so the two
    /// differ by roughly the preempt count.
    pub fn total() -> u64 {
        TOTAL.load(Ordering::Relaxed)
    }

    pub fn print() {
        let total = total();
        if total == 0 || !crate::kdiag_wake() {
            return;
        }
        logln!("== wakes by call site ({} total) ==", total);
        for i in 0..NR {
            let n = COUNTS[i].load(Ordering::Relaxed);
            if n == 0 {
                continue;
            }
            let pct = n as f64 * 100.0 / total as f64;
            let key = SITES[i].load(Ordering::Relaxed);
            if i == NR - 1 || key == 0 {
                logln!("==   {:<34} {:>9}  {:>5.1}%", "(other)", n, pct);
            } else {
                // Safety: only ever written from `note`, with a `&'static Location`.
                let loc: &'static Location<'static> =
                    unsafe { &*(key as *const Location<'static>) };
                let mut buf = alloc::string::String::new();
                use core::fmt::Write;
                let _ = write!(&mut buf, "{}:{}", loc.file(), loc.line());
                logln!("==   {:<34} {:>9}  {:>5.1}%", buf, n, pct);
            }
        }
    }
}

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

    /// Preempts declined because the target held a mutex and the waker was the same class.
    static HOLDER_SPARED: AtomicU64 = AtomicU64::new(0);

    pub fn holder_spared() {
        HOLDER_SPARED.fetch_add(1, Ordering::Relaxed);
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
    /// A thread's *first* wake -- `schedule_new_thread`, i.e. the tail of `sys_spawn`. Latency
    /// label only: these samples move out of `remote`/`local-*` rather than adding to the totals,
    /// and the preempt decision does not see them (see `lat_kind` in `schedule_thread_on_cpu`).
    pub const WAKE_NEW_LOCAL: u32 = 5;
    pub const WAKE_NEW_REMOTE: u32 = 6;
    const NR_KINDS: usize = 7;

    /// Calls to `schedule_new_thread`, so the two `new-*` histogram counts can be checked against
    /// the number of threads actually created. They will not match exactly -- a thread that exits
    /// before it is ever switched to never consumes its stamp -- but a large gap means the stamp
    /// is being lost, not that spawn is fast, and without this the histogram cannot tell those
    /// apart. A count with no denominator is not a measurement.
    static NEW_THREADS: AtomicU64 = AtomicU64::new(0);

    pub fn new_thread() {
        NEW_THREADS.fetch_add(1, Ordering::Relaxed);
    }

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
            "== preempts declined for a mutex holder (same class): {} ==",
            HOLDER_SPARED.load(Ordering::Relaxed)
        );
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
        logln!(
            "  new threads created: {} (see new-local + new-remote below)",
            NEW_THREADS.load(Ordering::Relaxed),
        );
        print_lat("new-local", WAKE_NEW_LOCAL as usize);
        print_lat("new-remote", WAKE_NEW_REMOTE as usize);
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
    let cp = current_processor();
    cp.stats.preempts.fetch_add(1, Ordering::Relaxed);
    schedule(SchedFlags::PREEMPT | SchedFlags::REINSERT);
}

pub fn schedule_hardtick() -> Option<u64> {
    let cp = current_processor();
    // Relaxed on purpose: a free-running counter with no other memory ordered against it.
    cp.stats.hardticks.fetch_add(1, Ordering::Relaxed);
    let cur = current_thread_ref()?;
    let (current_tick, diff) = cp.rq.hardtick();
    let cur_pri = cur.effective_priority();
    let ts_expire = cur.sched.pay_ticks(diff, cp.rq.timeslice(cur_pri.class));
    // After paying, so the granularity `needs_reschedule` reads counts this tick.
    let resched = needs_reschedule(true);
    let rq_pri = cp.rq.current_priority();
    // An expired slice with nothing queued has no one to rotate to.
    if resched || (ts_expire && !cp.rq.is_empty()) {
        if resched {
            cp.stats.preempt_pri.fetch_add(1, Ordering::Relaxed);
        } else {
            cp.stats.preempt_slice.fetch_add(1, Ordering::Relaxed);
        }
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
    if cp.rq.wake_pending() {
        return Some(WAKE_GRAN_TICKS);
    }
    Some(cp.rq.timeslice(rq_pri.max(cur_pri).class))
}

pub fn schedule_resched() {
    let cp = current_processor();
    cp.stats
        .wakeups
        .fetch_add(1, Ordering::Relaxed);
    let cur = current_thread_ref();
    let is_idle = cur.map_or(true, |t| t.is_idle_thread());
    // A critical thread makes `needs_reschedule` answer no -- which means "I cannot tell yet", not
    // "no reschedule is needed". Reading it as the latter discards the request outright, and
    // nothing retries: an IPI-delivered suspend against a running target then never takes.
    //
    // Mark anyway and let the consumer decide. `schedule_maybe_preempt` already leaves the flag
    // set when it cannot act on it, for exactly this reason, so the mark is taken at the first
    // moment the thread is not critical. This is the request side of that same fix.
    let cannot_tell = cur.is_some_and(|t| t.is_critical());
    if is_idle || cannot_tell || needs_reschedule(true) {
        schedule_mark_preempt();
    } else if cp.rq.wake_pending() && !cp.is_bsp() {
        crate::clock::schedule_oneshot_tick(WAKE_GRAN_TICKS);
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

pub fn schedule_stattick(dt: Nanoseconds) {
    schedule_maybe_rebalance(dt);

    let cp = current_processor();
    if cp.is_bsp() {
        STAT_TICKS.fetch_add(1, Ordering::Relaxed);
    }
    let cur = current_thread_ref();
    if let Some(cur) = cur {
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

            // Charge and re-rate the running thread. Its penalty may have moved, and this cpu's
            // advertised priority is what remote wakes compare against.
            crate::thread::cachemiss::charge_tick(&cur);
            cur.cachemiss.sample();
            cp.current_priority
                .store(cur.effective_priority().raw(), Ordering::Release);
        }
    }

    cp.rq.clock();
    cp.freq.tick();
}
