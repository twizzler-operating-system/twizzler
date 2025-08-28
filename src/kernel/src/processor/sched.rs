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
    clock::{get_current_ticks, Nanoseconds},
    interrupt,
    once::Once,
    processor::{
        mp::{all_processors, MAX_CPU_ID},
        Processor,
    },
    spinlock::Spinlock,
    thread::{current_thread_ref, priority::Priority, set_current_thread, Thread, ThreadRef},
    trace::{
        mgr::{is_thread_ktrace_thread, TraceEvent, TRACE_MGR},
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
                let jload = load * 256 - (quick_random() % 128) as u64;
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
            // Use some jitter.
            let jload = load * 256 - (quick_random() % 128) as u64;

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
    let should_signal = processor.id != current_processor().id
        && (processor.rq.is_empty()
            || processor.rq.current_priority() <= thread.effective_priority());
    thread.sched.moving_to_queue(processor.id);
    log::trace!(
        "insert thread {} -> {} ({})",
        thread.id(),
        processor.id,
        is_current
    );

    reset_thread_time(&thread, processor);
    processor.rq.insert(thread, is_current);
    //processor.rq.print();
    if should_signal {
        processor.wakeup(true);
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
            log::trace!(
                "{}: {} considering {} for steal (load = {})",
                core::panic::Location::caller(),
                us.id,
                processor.id,
                otherload
            );
            /* try to steal something */
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

fn balance(topo: &CPUTopoNode) {
    static BAL_LOCK: Spinlock<()> = Spinlock::new(());
    log::trace!("starting rebalance at {}", get_current_ticks());
    let _guard = BAL_LOCK.lock();
    for cpu in all_processors() {
        if let Some(cpu) = cpu {
            if cpu.is_running() {
                log::trace!("==> {}: {}", cpu.id, cpu.current_load());
            }
        }
    }

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

    return;

    /*
    /* TODO: maximum number of iterations? */
    while cpuset.count_ones(..) > 0 {
        let donor = find_cpu_from_topo(topo, true, None, Some(&cpuset))
            .expect("this should always give us a CPU");
        let recipient =
            find_cpu_from_topo(topo, false, None, None).expect("this should always give us a CPU");

        /* remove the recipient from the allowed donor list */
        cpuset.remove(recipient.cpuid);

        let donor = get_processor(donor.cpuid);
        let recipient = get_processor(recipient.cpuid);

        log::info!(
            "considering {} -> {} (loads {} {})",
            donor.id,
            recipient.id,
            donor.current_load(),
            recipient.current_load(),
        );

        let donor_load = donor.current_load();
        if donor_load <= 2 {
            break;
        }

        let thread = take_a_thread_from_cpu(donor, recipient.id);
        if let Some(thread) = thread {
            log::info!(
                "rebalanced {} ({} -> {})",
                thread.objid(),
                donor.id,
                recipient.id
            );
            schedule_thread_on_cpu(thread, recipient, false);
        } else {
            cpuset.set(donor.id as usize, false);
        }
    }
    */
}

fn select_cpu(thread: &ThreadRef, try_avoid: Option<u32>) -> u32 {
    /* TODO: restrict via cpu sets as step 0, and in global searches */
    /* TODO: take SMT into acount */
    let last_cpuid = thread
        .sched
        .preferred_cpu()
        .map(|(x, _p)| x as i32)
        .unwrap_or(-1);
    let mut last_load = None;
    /* 1: if the thread can run on the last CPU it ran on, and that CPU is idle, then do that. */
    if last_cpuid >= 0 && try_avoid.is_none_or(|ta| ta != last_cpuid as u32) {
        let processor = get_processor(last_cpuid as u32);
        last_load = Some(processor.current_load());
        if processor.rq.current_load() == 0 {
            return last_cpuid as u32;
        }
        if thread.effective_priority() > processor.current_priority() {
            return last_cpuid as u32;
        }
        log::trace!(
            "{}: last: {}: {} {:?} {:?}: {}",
            thread.id(),
            last_cpuid,
            processor.current_load(),
            thread.effective_priority(),
            processor.current_priority(),
            thread.effective_priority() >= processor.current_priority()
        );
    }

    /* 2: search for the least loaded that will run this thread immediately */
    let res = find_cpu_from_topo(
        get_cpu_topology(),
        false,
        Some(&thread.effective_priority()),
        None,
    );
    if let Some(res) = res {
        log::trace!(
            "{}: found(pri) {} with load {} ({:?})",
            thread.id(),
            res.cpuid,
            res.load,
            last_load,
        );
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
    let oldcpu = thread.sched.moving_to_active(cp.id);
    if old.id() != thread.id() {
        if thread.is_idle_thread() {
            //log::info!("switch to idle from {}", old.id());
            //cp.rq.print();
        }
        log::trace!(
            "switch {} <- {} ({} {})",
            thread.id(),
            old.id(),
            thread.is_idle_thread(),
            old.is_idle_thread(),
        );
        //cp.rq.print();
        trace_switch(&old, &thread, flags);
    }
    cp.stats.switches.fetch_add(1, Ordering::SeqCst);

    if let Some(oldcpu) = oldcpu {
        if oldcpu != cp.id {
            log::trace!("migrated {} {} -> {}", thread.id(), oldcpu, cp.id);
            trace_migrate(&thread, oldcpu as u64, cp.id as u64);
        }
    }

    if !thread.is_idle_thread() {
        cp.current_priority
            .store(thread.effective_priority().raw(), Ordering::SeqCst);
        cp.exit_idle();
        crate::clock::schedule_oneshot_tick(cp.rq.timeslice(thread.effective_priority().class));
    } else {
        cp.enter_idle();
        cp.current_priority.store(0, Ordering::SeqCst);
    }
    cp.reset_rebalance();
    unsafe { set_current_thread(&thread) };

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
    cur.enter_critical();

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
            if cpuid != processor.id {
                log::trace!(
                    "{} reinsert moved thread {} to {}",
                    processor.id,
                    cur.id(),
                    cpuid
                );
            } else {
                log::trace!("reinsert {} -> {} ({:?})", cur.id(), cpuid, flags);
            }
            let processor = get_processor(cpuid);
            schedule_thread_on_cpu(cur.clone(), processor, false);
        } else {
            // This is a current thread to reinsert, but only count it as such if it is not
            // yielding so that other threads will run first.
            if flags.contains(SchedFlags::YIELD) {
                log::trace!(
                    "yield reinsert {} -> {} ({:?})",
                    cur.id(),
                    processor.id,
                    flags
                );
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
        log::trace!(
            "took thread {} ({:?})",
            next.id(),
            next.effective_priority()
        );
        //processor.rq.print();
        if &next == cur {
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

    do_schedule(flags);
    interrupt::set(istate);
    let cur = current_thread_ref().unwrap();
    // Always check if we need to suspend before returning control.
    cur.maybe_suspend_self();
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
        return false;
    }
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

#[thread_local]
static PREEMPT: AtomicBool = AtomicBool::new(false);
pub fn schedule_mark_preempt() {
    PREEMPT.store(true, Ordering::SeqCst);
}

pub fn schedule_maybe_preempt() {
    if PREEMPT.swap(false, Ordering::SeqCst) {
        let cp = current_processor();
        cp.stats.preempts.fetch_add(1, Ordering::SeqCst);
        schedule(SchedFlags::PREEMPT | SchedFlags::REINSERT)
    }
}

pub fn schedule_hardtick() -> Option<u64> {
    let cp = current_processor();
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
        .fetch_add(1, Ordering::SeqCst);
    let is_idle = current_thread_ref().map_or(true, |t| t.is_idle_thread());
    if is_idle || needs_reschedule(false) {
        schedule_mark_preempt();
    }
}

#[thread_local]
static STAT_COUNTER: AtomicU64 = AtomicU64::new(0);
const PRINT_STATS: bool = false;
pub fn schedule_stattick(dt: Nanoseconds) {
    schedule_maybe_rebalance(dt);

    let s = STAT_COUNTER.fetch_add(1, Ordering::SeqCst);
    let cp = current_processor();
    let cur = current_thread_ref();
    if let Some(cur) = cur {
        if !cur.is_critical() && cur.is_in_user() {
            cp.cleanup_exited();
            // TODO: need to call this much more rarely, and not from within a scheduler tick.
            //TRACE_MGR.process_async_and_maybe_flush();
        }
        if cur.is_idle_thread() {
            cp.stats.idle.fetch_add(1, Ordering::SeqCst);
        } else {
            cp.stats.non_idle.fetch_add(1, Ordering::SeqCst);
            /* Update thread stats */
            if cur.is_in_user() {
                cur.stats.user.fetch_add(1, Ordering::SeqCst);
            } else {
                cur.stats.sys.fetch_add(1, Ordering::SeqCst);
            }

            //TODO user vs sys
            let diff = cur.stats.last.load(Ordering::SeqCst);
            cur.stats.idle.store(diff, Ordering::SeqCst);
            cur.stats.last.store(s, Ordering::SeqCst);
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
            //crate::memory::print_fault_stats();
        }
        //crate::clock::print_info();
    }
}
