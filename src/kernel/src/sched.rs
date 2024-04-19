use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use fixedbitset::FixedBitSet;
use twizzler_abi::thread::ExecutionState;

use crate::{
    clock::Nanoseconds,
    interrupt,
    once::Once,
    processor::{current_processor, get_processor, Processor},
    spinlock::Spinlock,
    thread::{current_thread_ref, priority::Priority, set_current_thread, Thread, ThreadRef},
    utils::quick_random,
};

#[derive(Clone, Debug, Copy)]
pub enum CPUTopoType {
    System,
    Cache,
    Thread,
    Other,
}

#[derive(Debug)]
pub struct CPUTopoNode {
    level_type: CPUTopoType,
    count: usize,
    cpuset: fixedbitset::FixedBitSet,
    first: usize,
    last: usize,
    children: Vec<CPUTopoNode>,
    parent: AtomicPtr<CPUTopoNode>,
}

impl CPUTopoNode {
    pub fn new(ty: CPUTopoType) -> CPUTopoNode {
        Self {
            cpuset: fixedbitset::FixedBitSet::new(),
            first: usize::MAX,
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
        let id = id as usize;
        self.cpuset.grow(core::cmp::max(id + 1, self.cpuset.len()));
        self.cpuset.insert(id);
        if self.first > id {
            self.first = id;
        }
        if self.last < id {
            self.last = id;
        }
        self.count += 1;
    }
}

static CPU_TOPOLOGY_ROOT: Once<CPUTopoNode> = Once::new();

pub fn set_cpu_topology(root: CPUTopoNode) {
    CPU_TOPOLOGY_ROOT.call_once(|| root);
}

pub fn get_cpu_topology() -> &'static CPUTopoNode {
    CPU_TOPOLOGY_ROOT.poll().unwrap()
}

struct SearchCPUResult {
    load: u64,
    cpuid: u32,
}

fn find_cpu_from_topo(
    node: &CPUTopoNode,
    highest: bool,
    pri: Option<&Priority>,
    allowed_set: Option<&FixedBitSet>,
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
                let skip = pri.map_or(false, |pri| &processor.current_priority() >= pri)
                    || allowed_set.map_or(false, |set| !set.contains(c));
                if skip {
                    continue;
                }
                let load = processor.current_load();
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

fn schedule_thread_on_cpu(thread: ThreadRef, processor: &Processor) {
    let mut sched = processor.schedlock();
    let should_signal = processor.id != current_processor().id
        && sched.should_preempt(&thread.effective_priority(), false);
    processor.load.fetch_add(1, Ordering::SeqCst);
    thread
        .current_processor_queue
        .store(processor.id as i32, Ordering::SeqCst);
    sched.reinsert_thread(thread);
    if should_signal {
        processor.wakeup(true);
    }
}

fn take_a_thread_from_cpu(processor: &Processor) -> Option<ThreadRef> {
    let mut sched = processor.schedlock();
    let thread = sched.choose_next(false);
    if let Some(ref thread) = thread {
        thread.current_processor_queue.store(-1, Ordering::SeqCst);
        processor.load.fetch_sub(1, Ordering::SeqCst);
    }
    thread
}

const STEAL_LOAD_THRESH: u64 = 3;
fn try_steal() -> Option<ThreadRef> {
    /* TODO: we need a cooldown on migration */
    let us = current_processor();
    let res = find_cpu_from_topo(get_cpu_topology(), true, None, None);
    if let Some(res) = res {
        let processor = get_processor(res.cpuid);
        let otherload = processor.current_load();
        if otherload > STEAL_LOAD_THRESH && otherload > (us.current_load() + 1) {
            /* try to steal something */
            let thread = take_a_thread_from_cpu(processor);
            if thread.is_some() {
                us.load.fetch_add(1, Ordering::SeqCst);
            }
            return thread;
        }
    }
    None
}

fn balance(topo: &CPUTopoNode) {
    static BAL_LOCK: Spinlock<()> = Spinlock::new(());
    let _guard = BAL_LOCK.lock();
    let mut cpuset = topo.cpuset.clone();
    /* TODO: maximum number of iterations? */
    while cpuset.count_ones(..) > 0 {
        let donor = find_cpu_from_topo(topo, true, None, Some(&cpuset))
            .expect("this should always give us a CPU");
        let recipient =
            find_cpu_from_topo(topo, false, None, None).expect("this should always give us a CPU");
        /* remove the recipient from the allowed donor list */
        cpuset.set(recipient.cpuid as usize, false);

        let donor = get_processor(donor.cpuid);
        let recipient = get_processor(recipient.cpuid);
        let donor_load = donor.current_load();
        // logln!("balance {:?} {}", cpuset, donor_load);
        if donor_load <= 2 {
            break;
        }

        let thread = take_a_thread_from_cpu(donor);
        if let Some(thread) = thread {
            schedule_thread_on_cpu(thread, recipient);
        } else {
            cpuset.set(donor.id as usize, false);
        }
    }
}

fn select_cpu(thread: &ThreadRef) -> u32 {
    /* TODO: restrict via cpu sets as step 0, and in global searches */
    /* TODO: take SMT into acount */
    let last_cpuid = thread.last_cpu.load(Ordering::Acquire);
    /* 1: if the thread can run on the last CPU it ran on, and that CPU is idle, then do that. */
    if last_cpuid >= 0 {
        let processor = get_processor(last_cpuid as u32);
        if processor.current_load() == 1 {
            return last_cpuid as u32;
        }
        if thread.effective_priority() >= processor.current_priority() {
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
        return res.cpuid;
    }

    /* 3: search for the least loaded */
    let res = find_cpu_from_topo(get_cpu_topology(), false, None, None)
        .expect("global CPU search should always produce results");

    res.cpuid
}

static ALL_THREADS: Spinlock<BTreeMap<u64, ThreadRef>> = Spinlock::new(BTreeMap::new());

pub fn remove_thread(id: u64) {
    ALL_THREADS.lock().remove(&id);
}

pub fn schedule_new_thread(thread: Thread) -> ThreadRef {
    thread.set_state(ExecutionState::Running);
    let thread = Arc::new(thread);
    {
        ALL_THREADS.lock().insert(thread.id(), thread.clone());
    }
    let cpuid = select_cpu(&thread);
    let processor = get_processor(cpuid);
    schedule_thread_on_cpu(thread.clone(), processor);
    thread
}

pub fn schedule_thread(thread: ThreadRef) {
    thread.set_state(ExecutionState::Running);
    if thread.is_idle_thread() {
        return;
    }
    let cpuid = select_cpu(&thread);
    let processor = get_processor(cpuid);
    schedule_thread_on_cpu(thread, processor);
}

pub fn create_idle_thread() {
    let idle = Arc::new(Thread::new_idle());
    current_processor().set_idle_thread(idle.clone());
    set_current_thread(idle);
}

fn switch_to(thread: ThreadRef, old: ThreadRef) {
    let cp = current_processor();
    cp.stats.switches.fetch_add(1, Ordering::SeqCst);
    set_current_thread(thread.clone());
    thread
        .last_cpu
        .store(current_processor().id as i32, Ordering::SeqCst);
    if !thread.is_idle_thread() {
        crate::clock::schedule_oneshot_tick(1);
    }
    /* Okay, so this is a little gross. Basically, we need to drop these references to make
    sure the refcounts don't climb every time we switch_to() with an exiting thread. But we still need a reference
    to the underlying thread so we can do the switch_thread call.

    So we manually decrement the refcounts while maintaining a raw pointer to the underlying.
    Why is this safe?
      1. For the old pointer, it's safe because this thread is either exiting, in which case it
         is placed in the exit queue for THIS CPU, thus we can ensure that the reference will not
         dangle, since that only gets cleaned up by THIS CPU on the next scheduling softtick. If
         the thread is not exiting, then it must be either sleeping, and on a queue somewhere else,
         or its on a different CPU queue. In both cases, the thread is on a different queue. Since
         the switch_thread function internally must lock to handle SMP cross-cpu scheduling, and
         after that lock is released, the old pointer is never used (this is part of the contract
         of the swtich_thread function), we know the old pointer will live at least as long as that
         lock is held through the switch_thread call, after which switch_to isn't allowed to look at
         it anyway. Thus, the pointer will not dangle.
      2. For the new (thread) pointer, we know this reference is safe because we just wrote it as
         the current thread pointer for this CPU, so we know it won't dangle. */
    let threadt = Arc::into_raw(thread);
    let oldt = Arc::into_raw(old);
    unsafe {
        Arc::decrement_strong_count(oldt);
        Arc::decrement_strong_count(threadt);
        threadt
            .as_ref()
            .unwrap()
            .switch_thread(oldt.as_ref().unwrap());
    }
}

fn do_schedule(reinsert: bool) {
    // TODO: remove the duplicate calls here
    let cur = current_thread_ref().unwrap();
    let processor = current_processor();
    cur.enter_critical();
    if !cur.is_idle_thread() && reinsert {
        schedule_thread(cur.clone());
    }
    if cur.is_exiting() {
        processor.push_exited(cur.clone());
    }
    if !cur.is_idle_thread() {
        let res = processor.load.fetch_sub(1, Ordering::SeqCst);
        assert!(res > 1);
    }
    let next = {
        let mut scheduler = processor.schedlock();
        scheduler.choose_next(true)
    };

    if let Some(next) = next {
        if next == cur {
            return;
        }
        next.current_processor_queue.store(-1, Ordering::SeqCst);
        switch_to(next, cur);
        return;
    }

    if let Some(stolen) = try_steal() {
        let cp = current_processor();
        cp.stats.steals.fetch_add(1, Ordering::SeqCst);
        switch_to(stolen, cur);
        return;
    }

    if cur.is_idle_thread() {
        return;
    }
    switch_to(processor.idle_thread.wait().clone(), cur);
}

pub fn schedule(reinsert: bool) {
    let cur = current_thread_ref().unwrap();
    /* TODO: switch to needs to also drop the ref on cur, somehow... */
    /* TODO: if we preempt, just put the thread back on our list (or decide to not resched) */
    let istate = interrupt::disable();
    if cur.is_critical() {
        interrupt::set(istate);
        return;
    }

    do_schedule(reinsert);
    interrupt::set(istate);
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
    if cur.must_suspend() {
        return true;
    }
    let sched = processor.schedlock();
    sched.should_preempt(&cur.effective_priority(), ticking)
}

#[thread_local]
static mut CUR_REBALANCE_TIME: Nanoseconds = 0;
const REBALANCE_TIME: Nanoseconds = 2000000000;

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
        schedule(true)
    }
}

pub fn schedule_hardtick() -> Option<u64> {
    let cp = current_processor();
    cp.stats.hardticks.fetch_add(1, Ordering::SeqCst);
    let resched = needs_reschedule(true);
    if resched {
        schedule_mark_preempt();
    }
    let cur = current_thread_ref()?;
    let notick = cur.is_idle_thread() && !resched;
    if notick {
        None
    } else {
        Some(1)
    }
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
    if let Some(ref cur) = cur {
        if !cur.is_critical() && (cur.is_in_user() || cur.is_idle_thread()) {
            cp.cleanup_exited();
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

    if PRINT_STATS && s % 200 == 0 {
        logln!(
            "STAT {}; {}({}): load {:2}, i {:4}, ni {:4}, sw {:4}, w {:4}, p {:4}, h {:4}, s {:4}",
            cp.id,
            cur.as_ref().unwrap().id(),
            cur.unwrap().is_idle_thread(),
            cp.current_load(),
            cp.stats.idle.load(Ordering::SeqCst),
            cp.stats.non_idle.load(Ordering::SeqCst),
            cp.stats.switches.load(Ordering::SeqCst),
            cp.stats.wakeups.load(Ordering::SeqCst),
            cp.stats.preempts.load(Ordering::SeqCst),
            cp.stats.hardticks.load(Ordering::SeqCst),
            cp.stats.steals.load(Ordering::SeqCst),
        );
        if cp.id == 0 {
            let all_threads = ALL_THREADS.lock();
            for t in all_threads.values() {
                logln!("thread {}: {:?} {:?}", t.id(), t.stats, t.get_state());
            }
        }
        //crate::clock::print_info();
    }
}
