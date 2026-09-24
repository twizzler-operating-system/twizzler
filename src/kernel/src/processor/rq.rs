use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use intrusive_collections::{LinkedList, intrusive_adapter};

use super::{
    sched::{DEFAULT_TIMESLICE_TICKS, MAX_TIMESLICE_TICKS, MIN_TIMESLICE_TICKS, wakestats},
    timeshare::TimeshareQueue,
};
use crate::{
    clock::get_current_ticks,
    spinlock::{GenericSpinlock, LockGuard},
    thread::{
        Thread, ThreadRef, current_thread_ref,
        interact::BATCH_MAX,
        priority::{MAX_PRIORITY, Priority, PriorityClass},
    },
};

pub const NR_QUEUES: usize = 8;
#[repr(transparent)]
struct SchedSpinlock<T>(GenericSpinlock<T>);

impl<T> SchedSpinlock<T> {
    fn lock(&self) -> SchedLockGuard<'_, T> {
        // The critical charge now comes from `GenericSpinlock::lock` itself, for every spinlock;
        // charging again here would double-count. Kept only to name the acquiring thread in the
        // crossing diagnostic below.
        let critical = current_thread_ref().map(|c| &**c);
        let queue = self.0.lock();
        SchedLockGuard { queue, critical }
    }
}

const RQ_HAS_RT: u32 = 1;
const RQ_HAS_TS: u32 = 2;
const RQ_HAS_IL: u32 = 4;

/// Which structure a [`RunQueue::remove_thread`] found its thread in. `Idle` is the one the
/// starvation diagnostics care about: it names the queue `take` cannot reach past a spinner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovedFrom {
    Realtime,
    Timeshare,
    Idle,
}

/// Zero-sized alignment boundary; see its use in [RunQueue].
#[repr(align(64))]
struct Align64;

#[repr(C)]
pub struct RunQueue<const N: usize> {
    realtime: SchedSpinlock<PriorityQueue<N>>,
    timeshare: SchedSpinlock<TimeshareQueue>,
    idle: SchedSpinlock<PriorityQueue<N>>,
    /// Everything below is read by every other cpu's `select_cpu` walk while the fields above
    /// are written under this cpu's locks. The boundary (with `repr(C)` making declaration
    /// order the layout) keeps the remotely-read group off the locks' lines now that packed
    /// tickets (`spinlock::Tickets`) shrank each lock; without it the wake path would inherit
    /// exactly the false sharing the packing removed elsewhere.
    _remote_read_split: Align64,
    current_priority: AtomicU32,
    flags: AtomicU32,
    /// Realtime-class threads in the realtime queue, which also holds boosted User threads.
    realtime_class: AtomicU32,
    load: AtomicU32,
    timeshare_load: AtomicU32,
    movable: AtomicU32,
    /// Threads queued by a wake (`ThreadSched::woken`) and not yet taken. See `needs_reschedule`.
    pending_wakes: AtomicU32,
    last_clock: AtomicU64,
    last_tick: AtomicU64,
}

#[must_use = "a dropped guard releases immediately; bind it to a variable"]
pub struct SchedLockGuard<'a, T> {
    pub(super) queue: LockGuard<'a, T>,
    /// DIAG: thread current at acquisition, for the crossing report only.
    critical: Option<&'static crate::thread::Thread>,
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
        use crate::thread::locktrack::diag;
        if let Some(critical) = self.critical {
            // DIAG: means the guard was held across a context switch.
            if diag::this_thread() != critical.id() && diag::SCHED_GUARD_CROSSED.hit() {
                emerglogln!(
                    "locktrack: sched lock {} entered critical on thread {}, dropped while thread {} current (cpu {})",
                    self.queue.locker,
                    critical.id(),
                    diag::this_thread(),
                    diag::this_cpu(),
                );
            }
        }
    }
}

/// Own cache line: this is a per-cpu lock payload in a static `Processor` array, so with
/// packed `spinlock::Tickets` an adjacent cpu's spin would otherwise land on this cpu's queue
/// data. Aligning the payload rather than the lock is what keeps it off the ticket line.
#[repr(align(64))]
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

    /// The priority of the thread `take` would return: bucket 0 holds both boosted User threads
    /// and low realtime values, so the bucket index alone would misreport a User thread as
    /// realtime to every preempt and placement decision.
    fn highest_priority(&self) -> Option<Priority> {
        if self.count == 0 {
            return None;
        }
        for q in (0..N).rev() {
            if let Some(front) = self.queues[q].front().get() {
                return Some(front.get_stable_effective_priority());
            }
        }
        None
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn insert(&mut self, th: ThreadRef) {
        let priority = th.get_stable_effective_priority();
        self.count += 1;
        if priority.class == PriorityClass::User {
            // Boosted User threads share the lowest realtime slot, so they beat the calendar
            // without cutting ahead of genuine realtime work. Within it they are ordered by value
            // and FIFO within a value, a preempted thread included: the wake rotation preempts a
            // runner so a queued equal can run, and a runner re-entering ahead of that equal is
            // taken straight back (pingpong at 12 threads on 4 cpus wedged on it). A front insert
            // for every interactive arrival made the slot a LIFO: freshly spawned spinners are
            // interactive by inheritance for their first second, and a sleeper filed behind them
            // waited that second out.
            Self::note(&th, wakestats::NOTE_RT_ORDERED, 0);
            // From the back: the lower-valued User threads are a suffix, and the usual arrival
            // (FIFO behind its equals, or the lowest) stops at once.
            let mut cursor = self.queues[0].back_mut();
            while let Some(t) = cursor.get() {
                let tp = t.get_stable_effective_priority();
                if tp.class != PriorityClass::User || tp.value >= priority.value {
                    break;
                }
                cursor.move_prev();
            }
            cursor.insert_after(th);
            return;
        }
        let q = priority.value as usize / (MAX_PRIORITY as usize / N);
        assert!(q < N, "priority value {} out of range", priority.value);
        Self::note(&th, wakestats::NOTE_RT_CLASS, q);
        if q == 0 {
            // Realtime values 0..16 share the slot with boosted User threads: ahead of all of
            // them, FIFO among themselves.
            let mut cursor = self.queues[0].front_mut();
            while let Some(t) = cursor.get() {
                if t.get_stable_effective_priority().class == PriorityClass::User {
                    break;
                }
                cursor.move_next();
            }
            cursor.insert_before(th);
            return;
        }
        self.queues[q].push_back(th);
    }

    fn note(th: &Thread, kind: u8, idx: usize) {
        if crate::kdiag_wake() {
            th.sched.queue_note.store(
                wakestats::queue_note(kind, th, 0, 0, idx),
                Ordering::Relaxed,
            );
        }
    }

    fn take(&mut self) -> Option<ThreadRef> {
        if self.count == 0 {
            return None;
        }
        // Descending: a higher bucket index is higher priority (see insert/highest_priority).
        for q in (0..N).rev() {
            if let Some(th) = self.queues[q].pop_front() {
                self.count -= 1;
                return Some(th);
            }
        }

        None
    }

    /// Unlink `th` from whichever bucket holds it, for a priority-driven re-file. Pointer
    /// identity, since two threads can share a priority.
    fn remove_thread(&mut self, th: &Thread) -> Option<ThreadRef> {
        for q in 0..N {
            let mut cursor = self.queues[q].front_mut();
            while let Some(t) = cursor.get() {
                if core::ptr::eq(t as *const Thread, th as *const Thread) {
                    let t = cursor.remove();
                    self.count -= 1;
                    return t;
                }
                cursor.move_next();
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
            _remote_read_split: Align64,
            current_priority: AtomicU32::new(0),
            flags: AtomicU32::new(0),
            realtime_class: AtomicU32::new(0),
            load: AtomicU32::new(0),
            timeshare_load: AtomicU32::new(0),
            last_clock: AtomicU64::new(0),
            last_tick: AtomicU64::new(0),
            movable: AtomicU32::new(0),
            pending_wakes: AtomicU32::new(0),
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

    /// Whether `insert` would file `th` in the realtime queue: a realtime thread, or a User
    /// thread that is interactive (`interact`), holds a donated priority, or is past its
    /// deadline (see `deadline`). These are the User threads that preempt a running batch
    /// thread at once; batch threads themselves only ever meet in the calendar. One predicate
    /// for the insert and for the wake path's preempt decision, so the two cannot disagree.
    pub fn files_realtime(&self, th: &Thread) -> bool {
        let pri = th.effective_priority();
        match pri.class {
            PriorityClass::Realtime => true,
            PriorityClass::User => {
                th.interact.is_interactive()
                    || th.get_donated_priority().is_some()
                    || th.sched.get_deadline() <= get_current_ticks()
            }
            _ => false,
        }
    }

    pub fn has_realtime(&self) -> bool {
        self.flags.load(Ordering::Acquire) & RQ_HAS_RT != 0
    }

    /// A realtime-*class* thread is queued, as opposed to a User thread filed in the realtime
    /// queue by `files_realtime`.
    pub fn has_realtime_class(&self) -> bool {
        self.realtime_class.load(Ordering::Acquire) != 0
    }

    fn note_realtime_class(&self, th: &Thread, delta: i32) {
        if th.get_stable_effective_priority().class == PriorityClass::Realtime {
            if delta > 0 {
                self.realtime_class.fetch_add(1, Ordering::AcqRel);
            } else {
                let _ =
                    self.realtime_class
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
            }
        }
    }

    pub fn wake_pending(&self) -> bool {
        self.pending_wakes.load(Ordering::Acquire) != 0
    }

    fn untrack_wake(&self, th: &Thread) {
        if th.sched.take_woken() {
            let _ = self
                .pending_wakes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
        }
    }

    /// `wake`: a timeshare thread made runnable (not reinserted or migrated) goes to the front
    /// of the calendar and is counted in `pending_wakes`. `preempted`: a reinsertion of a thread
    /// that lost its cpu mid-slice, which also resumes at the front.
    pub fn insert(&self, th: ThreadRef, wake: bool, preempted: bool) -> bool {
        assert!(!th.is_idle_thread());
        if th.sched.note_queued_movable() {
            self.movable.fetch_add(1, Ordering::SeqCst);
        }
        self.load.fetch_add(1, Ordering::SeqCst);
        let th_pri = th.stable_effective_priority();
        // `raw` is class-major, so its integer max is the priority max; a load and a store let a
        // concurrent insert of a lower priority overwrite a higher one.
        self.current_priority
            .fetch_max(th_pri.raw(), Ordering::AcqRel);

        match th_pri.class {
            PriorityClass::Realtime => {
                self.note_realtime_class(&th, 1);
                let mut realtime = self.realtime.lock();
                realtime.insert(th);
                self.flags.fetch_or(RQ_HAS_RT, Ordering::SeqCst);
                true
            }
            PriorityClass::User => {
                if self.files_realtime(&th) {
                    if wake {
                        th.sched.set_woken();
                        self.pending_wakes.fetch_add(1, Ordering::AcqRel);
                    }
                    let mut realtime = self.realtime.lock();
                    realtime.insert(th);
                    self.flags.fetch_or(RQ_HAS_RT, Ordering::SeqCst);
                } else {
                    self.timeshare_load.fetch_add(1, Ordering::Release);
                    let mut timeshare = self.timeshare.lock();
                    if wake {
                        th.sched.set_woken();
                        self.pending_wakes.fetch_add(1, Ordering::AcqRel);
                        timeshare.insert_front(th);
                    } else {
                        timeshare.insert(th, preempted);
                    }
                    self.flags.fetch_or(RQ_HAS_TS, Ordering::SeqCst);
                }
                true
            }
            _ => {
                let mut idle = self.idle.lock();
                idle.insert(th);
                self.flags.fetch_or(RQ_HAS_IL, Ordering::SeqCst);
                false
            }
        }
    }

    fn recalc_priority_timeshare(&self, queue: SchedLockGuard<TimeshareQueue>) {
        if self.current_priority().class == PriorityClass::User {
            // Boosted User threads still queued in the realtime queue keep what they advertised.
            if queue.is_empty() && self.flags.load(Ordering::Acquire) & RQ_HAS_RT != 0 {
                return;
            }
            if queue.is_empty() {
                let priority = self
                    .idle
                    .lock()
                    .highest_priority()
                    .unwrap_or(Priority::from_raw(0));
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
        // Set and cleared only under the queue lock; a flag left on an empty queue made every
        // tick preempt the batch runner for nothing.
        let Some(th) = realtime.take() else {
            self.flags.fetch_and(!RQ_HAS_RT, Ordering::Release);
            return None;
        };
        self.note_realtime_class(&th, -1);
        if realtime.is_empty() {
            self.flags.fetch_and(!RQ_HAS_RT, Ordering::Release);
        }
        if th.sched.queued_movable() {
            let old = self.movable.fetch_sub(1, Ordering::SeqCst);
            assert!(old > 0);
        }
        self.load.fetch_sub(1, Ordering::Release);
        // Boosted User threads come off this queue too, and taking the last one must not leave
        // its value advertised.
        {
            if realtime.is_empty() {
                // Fall back to whatever the other queues actually hold, rather than always
                // claiming User priority -- otherwise an empty CPU keeps reporting itself busy
                // until some later take() reaches take_timeshare/take_idle. Derived from the
                // flags bitmask so this stays lock-free; a precise value would mean locking
                // timeshare/idle from here, which isn't worth it on this path.
                let f = self.flags.load(Ordering::Acquire);
                let priority = if f & RQ_HAS_TS != 0 {
                    // The calendar only holds batch values.
                    Priority {
                        value: BATCH_MAX,
                        class: PriorityClass::User,
                    }
                } else if f & RQ_HAS_IL != 0 {
                    Priority {
                        value: MAX_PRIORITY - 1,
                        class: PriorityClass::Idle,
                    }
                } else {
                    Priority::from_raw(0)
                };
                self.current_priority
                    .store(priority.raw(), Ordering::SeqCst);
            } else {
                let priority = realtime.highest_priority().unwrap_or(Priority::from_raw(0));
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
        if th.sched.queued_movable() {
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
        let Some(th) = idle.take() else {
            self.flags.fetch_and(!RQ_HAS_IL, Ordering::Release);
            return None;
        };
        if idle.is_empty() {
            self.flags.fetch_and(!RQ_HAS_IL, Ordering::Release);
        }
        if th.sched.queued_movable() {
            let old = self.movable.fetch_sub(1, Ordering::SeqCst);
            assert!(old > 0);
        }
        self.load.fetch_sub(1, Ordering::Release);
        let priority = idle.highest_priority().unwrap_or(Priority::from_raw(0));
        self.current_priority
            .store(priority.raw(), Ordering::SeqCst);
        Some(th)
    }

    /// Unlink `th` from this queue so it can be re-inserted under a raised priority.
    ///
    /// The queues bucket a thread by the priority it had at insert, and `take` never reaches a
    /// lower class while a higher one has work -- so a donation that crosses classes leaves its
    /// target stranded (a Background thread in the idle-class queue starves forever behind any
    /// spinning User thread). Mirrors the take_* bookkeeping
    /// exactly, except `current_priority` is left alone: stale-high is the safe direction, and
    /// the caller re-inserts immediately, which raises it as needed.
    ///
    /// Returns the reference still held by the queue and which structure held it -- the source
    /// matters to the caller's diagnostics, since a re-file out of the idle-class queue is the
    /// starvation case this exists for. The caller must pass the reference to `schedule_thread`
    /// (never drop it under a spinlock -- `Thread::drop` takes sleeping mutexes).
    pub fn remove_thread(&self, th: &Thread) -> Option<(ThreadRef, RemovedFrom)> {
        let (removed, from) = {
            let mut realtime = self.realtime.lock();
            let removed = realtime.remove_thread(th);
            if let Some(removed) = &removed {
                self.note_realtime_class(removed, -1);
                if realtime.is_empty() {
                    self.flags.fetch_and(!RQ_HAS_RT, Ordering::Release);
                }
            }
            (removed, RemovedFrom::Realtime)
        };
        let (removed, from) = match removed {
            Some(r) => (Some(r), from),
            None => {
                let mut timeshare = self.timeshare.lock();
                let removed = timeshare.remove_thread(th);
                if removed.is_some() {
                    self.timeshare_load.fetch_sub(1, Ordering::Release);
                    if timeshare.is_empty() {
                        self.flags.fetch_and(!RQ_HAS_TS, Ordering::Release);
                    }
                }
                (removed, RemovedFrom::Timeshare)
            }
        };
        let (removed, from) = match removed {
            Some(r) => (Some(r), from),
            None => {
                let mut idle = self.idle.lock();
                let removed = idle.remove_thread(th);
                if removed.is_some() && idle.is_empty() {
                    self.flags.fetch_and(!RQ_HAS_IL, Ordering::Release);
                }
                (removed, RemovedFrom::Idle)
            }
        };
        let removed = removed?;
        if removed.sched.queued_movable() {
            let old = self.movable.fetch_sub(1, Ordering::SeqCst);
            assert!(old > 0);
        }
        self.load.fetch_sub(1, Ordering::Release);
        self.untrack_wake(&removed);
        Some((removed, from))
    }

    pub fn take(&self, stealing: bool) -> Option<ThreadRef> {
        let th = self.take_inner(stealing)?;
        self.untrack_wake(&th);
        Some(th)
    }

    fn take_inner(&self, stealing: bool) -> Option<ThreadRef> {
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

    /// Whether everything queued is in a class `take` serves after `class`. The realtime queue
    /// also holds boosted User threads, so a Realtime caller counts any entry there.
    pub fn only_below(&self, class: PriorityClass) -> bool {
        let f = self.flags.load(Ordering::Acquire);
        match class {
            PriorityClass::Realtime => f & RQ_HAS_RT == 0,
            PriorityClass::User => f & (RQ_HAS_RT | RQ_HAS_TS) == 0,
            PriorityClass::Background | PriorityClass::Idle => {
                f & (RQ_HAS_IL | RQ_HAS_RT | RQ_HAS_TS) == 0
            }
        }
    }

    pub fn timeslice(&self, class: PriorityClass) -> u64 {
        match class {
            PriorityClass::User => {
                // Every queued thread, not the calendar alone: a spinner boosted into the realtime
                // slot competes for this cpu as much as one in the calendar, and counting only the
                // calendar gave the running thread the maximum slice whenever all were boosted.
                let load = self.load.load(Ordering::Acquire);
                if load == 0 {
                    return MAX_TIMESLICE_TICKS as u64;
                }
                (DEFAULT_TIMESLICE_TICKS / load).max(MIN_TIMESLICE_TICKS) as u64
            }
            _ => MAX_TIMESLICE_TICKS as u64,
        }
    }

    /// How long a thread taking the cpu now may be off it before an insert boosts it to the
    /// realtime queue: a slice for every thread queued ahead of it, and at least one -- a
    /// thread that ran alone would otherwise be given `now`, and be boosted on its very first
    /// reinsertion.
    pub fn deadline(&self, class: PriorityClass) -> u64 {
        self.timeslice(class) * self.current_load().max(1)
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
        self.timeshare.lock().clock();
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

mod test {
    use alloc::{sync::Arc, vec::Vec};

    use twizzler_kernel_macros::kernel_test;

    use super::{NR_QUEUES, PriorityQueue};
    use crate::thread::{
        Thread, ThreadRef,
        priority::{MAX_PRIORITY, Priority, PriorityClass},
    };

    const BUCKET_WIDTH: u16 = MAX_PRIORITY / NR_QUEUES as u16;

    fn thread_at(class: PriorityClass, value: u16) -> ThreadRef {
        let thread = Arc::new(Thread::new(None, None, Priority { class, value }));
        thread.set_name("idle");
        thread
    }

    fn drain(pq: &mut PriorityQueue<NR_QUEUES>) -> Vec<Priority> {
        let mut out = Vec::new();
        while let Some(th) = pq.take() {
            out.push(th.effective_priority());
        }
        out
    }

    #[kernel_test]
    fn test_priority_queue_take_is_descending() {
        let mut pq = PriorityQueue::<NR_QUEUES>::new();
        // One thread per bucket, inserted scrambled so the result can't come from insert order.
        let insert_order: [u16; NR_QUEUES] = [3, 0, 7, 5, 1, 6, 2, 4];
        for b in insert_order {
            pq.insert(thread_at(PriorityClass::Realtime, b * BUCKET_WIDTH));
        }
        assert_eq!(
            pq.highest_priority().map(|p| p.value),
            Some(7 * BUCKET_WIDTH)
        );

        let got: Vec<u16> = drain(&mut pq).iter().map(|p| p.value).collect();
        let expected: Vec<u16> = (0..NR_QUEUES as u16)
            .rev()
            .map(|b| b * BUCKET_WIDTH)
            .collect();
        assert_eq!(got, expected);
        assert!(pq.is_empty());
    }

    #[kernel_test]
    fn test_priority_queue_deadline_boost_is_lowest() {
        let mut pq = PriorityQueue::<NR_QUEUES>::new();
        // Boosted thread inserted first, so a FIFO regression would surface it first.
        pq.insert(thread_at(PriorityClass::User, MAX_PRIORITY / 2));
        pq.insert(thread_at(PriorityClass::Realtime, MAX_PRIORITY / 2));
        pq.insert(thread_at(PriorityClass::Realtime, MAX_PRIORITY - 1));

        let got = drain(&mut pq);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            Priority {
                class: PriorityClass::Realtime,
                value: MAX_PRIORITY - 1
            }
        );
        assert_eq!(
            got[1],
            Priority {
                class: PriorityClass::Realtime,
                value: MAX_PRIORITY / 2
            }
        );
        assert_eq!(got[2].class, PriorityClass::User);
    }

    #[kernel_test]
    fn test_remove_thread_from_idle_class() {
        use super::RunQueue;
        // A Background thread files into the idle-class queue; removing it must undo every
        // counter insert touched, or the queue reports load with nothing to take.
        let rq = RunQueue::<NR_QUEUES>::new();
        let bg = thread_at(PriorityClass::Background, MAX_PRIORITY / 2);
        rq.insert(bg.clone(), false, false);
        assert_eq!(rq.current_load(), 1);
        let (removed, from) = rq.remove_thread(&bg).expect("queued thread must be found");
        assert_eq!(removed.id(), bg.id());
        assert_eq!(from, super::RemovedFrom::Idle);
        assert_eq!(rq.current_load(), 0);
        assert_eq!(rq.movable(), 0);
        assert!(rq.is_empty());
        assert!(rq.take(false).is_none());
        // Re-inserting the removed reference must work (the re-file path).
        rq.insert(removed, false, false);
        assert_eq!(rq.current_load(), 1);
        assert_eq!(rq.take(false).unwrap().id(), bg.id());
    }

    #[kernel_test]
    fn test_remove_thread_from_timeshare() {
        use super::RunQueue;
        use crate::clock::get_current_ticks;
        let rq = RunQueue::<NR_QUEUES>::new();
        let user = thread_at(PriorityClass::User, MAX_PRIORITY / 2);
        // An unexpired deadline keeps the insert on the timeshare arm rather than the
        // deadline-boost one.
        user.sched.set_deadline(get_current_ticks() + 1_000_000);
        rq.insert(user.clone(), false, false);
        assert_eq!(rq.current_timeshare_load(), 1);
        let (removed, from) = rq
            .remove_thread(&user)
            .expect("queued thread must be found");
        assert_eq!(removed.id(), user.id());
        assert_eq!(from, super::RemovedFrom::Timeshare);
        assert_eq!(rq.current_timeshare_load(), 0);
        assert_eq!(rq.current_load(), 0);
        assert!(rq.is_empty());
        drop(removed);
        // A miss must be a clean None, with nothing decremented.
        let other = thread_at(PriorityClass::User, MAX_PRIORITY / 2);
        assert!(rq.remove_thread(&other).is_none());
        assert_eq!(rq.current_load(), 0);
    }

    #[kernel_test]
    fn test_priority_queue_fifo_within_bucket() {
        let mut pq = PriorityQueue::<NR_QUEUES>::new();
        // Both values land in the same bucket, so only arrival order can break the tie.
        let first = thread_at(PriorityClass::Realtime, MAX_PRIORITY / 2);
        let second = thread_at(PriorityClass::Realtime, MAX_PRIORITY / 2 + 1);
        let (first_id, second_id) = (first.id(), second.id());
        pq.insert(first);
        pq.insert(second);

        assert_eq!(pq.take().unwrap().id(), first_id);
        assert_eq!(pq.take().unwrap().id(), second_id);
        assert!(pq.take().is_none());
    }

    #[kernel_test]
    fn test_user_slot_orders_by_value_fifo_within() {
        let mut pq = PriorityQueue::<NR_QUEUES>::new();
        let a = thread_at(PriorityClass::User, 126);
        let b = thread_at(PriorityClass::User, 126);
        let sleeper = thread_at(PriorityClass::User, 127);
        let c = thread_at(PriorityClass::User, 126);
        let (a_id, b_id, s_id, c_id) = (a.id(), b.id(), sleeper.id(), c.id());
        pq.insert(a);
        pq.insert(b);
        pq.insert(sleeper);
        pq.insert(c);
        assert_eq!(pq.highest_priority().map(|p| p.value), Some(127));
        assert_eq!(pq.take().unwrap().id(), s_id);
        assert_eq!(pq.take().unwrap().id(), a_id);
        assert_eq!(pq.take().unwrap().id(), b_id);
        assert_eq!(pq.take().unwrap().id(), c_id);
        assert!(pq.take().is_none());
    }
}
