use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use intrusive_collections::{intrusive_adapter, LinkedList};

use super::{
    sched::{DEFAULT_TIMESLICE_TICKS, MAX_TIMESLICE_TICKS, MIN_TIMESLICE_TICKS},
    timeshare::TimeshareQueue,
};
use crate::{
    clock::get_current_ticks,
    spinlock::{GenericSpinlock, LockGuard, SpinLoop},
    thread::{
        current_thread_ref,
        priority::{Priority, PriorityClass, MAX_PRIORITY},
        Thread, ThreadRef,
    },
};

pub const NR_QUEUES: usize = 8;
#[repr(transparent)]
struct SchedSpinlock<T>(GenericSpinlock<T, SpinLoop>);

impl<T> SchedSpinlock<T> {
    fn lock(&self) -> SchedLockGuard<'_, T> {
        current_thread_ref().map(|c| c.enter_critical_unguarded());
        let queue = self.0.lock();
        SchedLockGuard { queue }
    }
}

const RQ_HAS_RT: u32 = 1;
const RQ_HAS_TS: u32 = 2;
const RQ_HAS_IL: u32 = 4;

pub struct RunQueue<const N: usize> {
    realtime: SchedSpinlock<PriorityQueue<N>>,
    timeshare: SchedSpinlock<TimeshareQueue<N>>,
    idle: SchedSpinlock<PriorityQueue<N>>,
    current_priority: AtomicU32,
    flags: AtomicU32,
    load: AtomicU32,
    timeshare_load: AtomicU32,
    movable: AtomicU32,
    last_clock: AtomicU64,
    last_tick: AtomicU64,
}

pub struct SchedLockGuard<'a, T> {
    pub(super) queue: LockGuard<'a, T, SpinLoop>,
}

impl<T> core::ops::Deref for SchedLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &*self.queue
    }
}

impl<T> core::ops::DerefMut for SchedLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.queue
    }
}

impl<T> Drop for SchedLockGuard<'_, T> {
    fn drop(&mut self) {
        current_thread_ref().map(|c| c.exit_critical(self.queue.locker));
    }
}

struct PriorityQueue<const N: usize> {
    count: usize,
    queues: [LinkedList<SchedLinkAdapter>; N],
}

impl<const N: usize> core::fmt::Debug for PriorityQueue<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "pq {:5} [ ", self.count)?;
        for i in 0..N {
            if i != 0 {
                write!(f, " | ")?;
            }
            let mut iter = self.queues[i].iter();
            if let Some(first) = iter.next() {
                if iter.next().is_some() {
                    write!(f, "{:5}...", first.id())?;
                } else {
                    write!(f, "{:5}   ", first.id())?;
                }
            } else {
                write!(f, "        ",)?;
            }
        }
        write!(f, "]")?;

        Ok(())
    }
}

impl<const N: usize> PriorityQueue<N> {
    const fn new() -> Self {
        const VAL: LinkedList<SchedLinkAdapter> = LinkedList::new(SchedLinkAdapter::NEW);
        Self {
            queues: [VAL; N],
            count: 0,
        }
    }

    fn highest_priority(&self) -> Option<u16> {
        if self.count == 0 {
            return None;
        }
        for q in (0..N).rev() {
            if !self.queues[q].is_empty() {
                return Some((q * (MAX_PRIORITY as usize / N)) as u16);
            }
        }
        None
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn insert(&mut self, th: ThreadRef) {
        let priority = th.effective_priority();
        let q = if priority.class == PriorityClass::User {
            // This must be a user thread getting a deadline boost.
            N - 1
        } else {
            priority.value as usize / (MAX_PRIORITY as usize / N)
        };
        if q > 8 {
            logln!("{} ==> {}", q, priority.value);
        }
        self.queues[q].push_back(th);
        self.count += 1;
    }

    fn take(&mut self) -> Option<ThreadRef> {
        if self.count == 0 {
            return None;
        }
        for q in 0..N {
            if let Some(th) = self.queues[q].pop_front() {
                self.count -= 1;
                return Some(th);
            }
        }

        None
    }
}

intrusive_adapter!(pub SchedLinkAdapter = ThreadRef: Thread { sched_link: intrusive_collections::linked_list::AtomicLink });

impl<const N: usize> RunQueue<N> {
    pub fn new() -> Self {
        Self {
            realtime: SchedSpinlock(GenericSpinlock::new(PriorityQueue::new())),
            timeshare: SchedSpinlock(GenericSpinlock::new(TimeshareQueue::new())),
            idle: SchedSpinlock(GenericSpinlock::new(PriorityQueue::new())),
            current_priority: AtomicU32::new(0),
            flags: AtomicU32::new(0),
            load: AtomicU32::new(0),
            timeshare_load: AtomicU32::new(0),
            last_clock: AtomicU64::new(0),
            last_tick: AtomicU64::new(0),
            movable: AtomicU32::new(0),
        }
    }

    pub fn print(&self) {
        logln!(
            "RUNQUEUE: {:x} {} {:?}",
            self.flags.load(Ordering::SeqCst),
            self.load.load(Ordering::SeqCst),
            self.current_priority()
        );
        logln!(" realtime: {:?}", &*self.realtime.lock());
        logln!("timeshare: {:?}", &*self.timeshare.lock());
        logln!("     idle: {:?}", &*self.idle.lock());
    }

    pub fn insert(&self, th: ThreadRef, current: bool) -> bool {
        assert!(!th.is_idle_thread());
        if th.sched.pinned_to().is_none() {
            self.movable.fetch_add(1, Ordering::SeqCst);
        }
        self.load.fetch_add(1, Ordering::SeqCst);
        let th_pri = th.effective_priority();
        let cur_pri = self.current_priority.load(Ordering::Acquire);
        if Priority::from_raw(cur_pri) < th_pri {
            self.current_priority.store(th_pri.raw(), Ordering::Release);
        }

        match th_pri.class {
            PriorityClass::Realtime => {
                self.realtime.lock().insert(th);
                self.flags.fetch_or(RQ_HAS_RT, Ordering::SeqCst);
                true
            }
            PriorityClass::User => {
                let is_thread_deadline = th.sched.get_deadline() <= get_current_ticks();
                if is_thread_deadline {
                    log::trace!(
                        "thread {} expired deadline ({} {})",
                        th.id(),
                        th.sched.get_deadline(),
                        get_current_ticks()
                    );
                    self.realtime.lock().insert(th);
                    self.flags.fetch_or(RQ_HAS_RT, Ordering::SeqCst);
                } else {
                    self.timeshare_load.fetch_add(1, Ordering::Release);
                    self.timeshare.lock().insert(th, current);
                    self.flags.fetch_or(RQ_HAS_TS, Ordering::SeqCst);
                }
                true
            }
            _ => {
                self.idle.lock().insert(th);
                self.flags.fetch_or(RQ_HAS_IL, Ordering::SeqCst);
                false
            }
        }
    }

    fn recalc_priority_timeshare(&self, queue: SchedLockGuard<TimeshareQueue<N>>) {
        if self.current_priority().class == PriorityClass::User {
            if queue.is_empty() {
                let priority = Priority {
                    value: self.idle.lock().highest_priority().unwrap_or(0),
                    class: PriorityClass::Idle,
                };
                self.current_priority
                    .store(priority.raw(), Ordering::SeqCst);
            } else {
                let priority = Priority {
                    value: queue.highest_priority().unwrap_or(0),
                    class: PriorityClass::User,
                };
                self.current_priority
                    .store(priority.raw(), Ordering::SeqCst);
            }
        }
    }

    fn take_realtime(&self) -> Option<ThreadRef> {
        if self.flags.load(Ordering::Acquire) & RQ_HAS_RT == 0 {
            return None;
        }
        let mut realtime = self.realtime.lock();
        let th = realtime.take()?;
        if realtime.is_empty() {
            self.flags.fetch_and(!RQ_HAS_RT, Ordering::Release);
        }
        if th.sched.pinned_to().is_none() {
            let old = self.movable.fetch_sub(1, Ordering::SeqCst);
            assert!(old > 0);
        }
        self.load.fetch_sub(1, Ordering::Release);
        if self.current_priority().class == PriorityClass::Realtime {
            if realtime.is_empty() {
                let priority = Priority {
                    value: MAX_PRIORITY - 1,
                    class: PriorityClass::User,
                };
                self.current_priority
                    .store(priority.raw(), Ordering::SeqCst);
            } else {
                let priority = Priority {
                    value: realtime.highest_priority().unwrap_or(0),
                    class: PriorityClass::Realtime,
                };
                self.current_priority
                    .store(priority.raw(), Ordering::SeqCst);
            }
        }
        Some(th)
    }

    fn take_timeshare(&self) -> Option<ThreadRef> {
        if self.flags.load(Ordering::Acquire) & RQ_HAS_TS == 0 {
            return None;
        }
        let mut timeshare = self.timeshare.lock();
        let th = timeshare.take()?;
        if timeshare.is_empty() {
            self.flags.fetch_and(!RQ_HAS_TS, Ordering::Release);
        }
        if th.sched.pinned_to().is_none() {
            let old = self.movable.fetch_sub(1, Ordering::SeqCst);
            assert!(old > 0);
        }
        self.load.fetch_sub(1, Ordering::Release);
        self.timeshare_load.fetch_sub(1, Ordering::Release);
        if self.current_priority().class == PriorityClass::User {
            self.recalc_priority_timeshare(timeshare);
        }
        Some(th)
    }

    fn take_idle(&self) -> Option<ThreadRef> {
        if self.flags.load(Ordering::Acquire) & RQ_HAS_IL == 0 {
            return None;
        }
        let mut idle = self.idle.lock();
        let th = idle.take()?;
        if idle.is_empty() {
            self.flags.fetch_and(!RQ_HAS_IL, Ordering::Release);
        }
        if th.sched.pinned_to().is_none() {
            let old = self.movable.fetch_sub(1, Ordering::SeqCst);
            assert!(old > 0);
        }
        self.load.fetch_sub(1, Ordering::Release);
        let priority = Priority {
            value: idle.highest_priority().unwrap_or(0),
            class: PriorityClass::Idle,
        };
        self.current_priority
            .store(priority.raw(), Ordering::SeqCst);
        Some(th)
    }

    pub fn take(&self, stealing: bool) -> Option<ThreadRef> {
        if self.is_empty() || (stealing && self.movable.load(Ordering::Acquire) == 0) {
            return None;
        }

        if let Some(th) = self.take_realtime() {
            return Some(th);
        }

        if let Some(th) = self.take_timeshare() {
            return Some(th);
        }

        if let Some(th) = self.take_idle() {
            return Some(th);
        }

        self.current_priority.store(0, Ordering::Release);
        None
    }

    pub fn is_empty(&self) -> bool {
        self.flags.load(Ordering::Acquire) & (RQ_HAS_IL | RQ_HAS_RT | RQ_HAS_TS) == 0
    }

    pub fn timeslice(&self, class: PriorityClass) -> u64 {
        match class {
            PriorityClass::User => {
                let load = self.timeshare_load.load(Ordering::Acquire);
                if load == 0 {
                    return MAX_TIMESLICE_TICKS as u64;
                }
                (DEFAULT_TIMESLICE_TICKS / load).max(MIN_TIMESLICE_TICKS) as u64
            }
            _ => MAX_TIMESLICE_TICKS as u64,
        }
    }

    pub fn deadline(&self, class: PriorityClass) -> u64 {
        self.timeslice(class) * self.current_load()
    }

    pub fn last_tick(&self) -> u64 {
        self.last_tick.load(Ordering::Acquire)
    }

    pub fn hardtick(&self) -> (u64, u64) {
        let current_ticks = get_current_ticks();
        let ticks = current_ticks - self.last_tick.load(Ordering::Acquire);
        if ticks == 0 {
            return (current_ticks, 0);
        }
        self.last_tick.fetch_add(ticks, Ordering::Release);
        (current_ticks, ticks)
    }

    pub fn clock(&self) {
        self.timeshare.lock().advance_insert_index(1, true);
    }

    pub fn current_priority(&self) -> Priority {
        Priority::from_raw(self.current_priority.load(Ordering::SeqCst))
    }

    pub fn current_load(&self) -> u64 {
        self.load.load(Ordering::Acquire) as u64
    }

    pub fn current_timeshare_load(&self) -> u64 {
        self.timeshare_load.load(Ordering::Acquire) as u64
    }

    pub fn movable(&self) -> u32 {
        self.movable.load(Ordering::Acquire)
    }
}
