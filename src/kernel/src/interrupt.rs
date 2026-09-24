use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use intrusive_collections::RBTree;
use twizzler_abi::{
    device::{CacheType, DeviceRepr},
    kso::{InterruptAllocateOptions, InterruptPriority},
    object::Protections,
    syscall::MapFlags,
};

use crate::{
    arch::{
        self,
        interrupt::{InterProcessorInterrupt, MAX_VECTOR, NUM_VECTORS},
    },
    condvar::CondVar,
    memory::context::{
        KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, kernel_context,
        virtmem::KernelObjectVirtHandle,
    },
    obj::{ObjectRef, thread_sync::ThreadSleepAdapter},
    once::Once,
    processor::sched::schedule_maybe_preempt,
    spinlock::Spinlock,
    syscall::sync::{add_to_requeue, requeue_all},
    thread::{ThreadRef, current_thread_ref, priority::Priority},
};

/// Set the current interrupt enable state to disabled and return the old state.
// `inline(always)`, not `inline`: at opt-level 0 the ordinary inliner does not run at all, only
// LLVM's always-inliner, so a plain `#[inline]` leaves this a real call in the debug build. These
// three are a handful of instructions each and sit under every spinlock acquire and every critical
// section, which is exactly where the debug profile spends its time.
//
// `compiler_fence`, not `fence`. What these have to guarantee is that the region a caller masks
// interrupts around does not leak out of it -- that the compiler does not hoist a load above the
// `cli` or sink a store below the `sti`. That is a compile-time property, and `compiler_fence`
// emits nothing to enforce it. A `fence(SeqCst)` additionally emits an `mfence`, which buys
// nothing here: masking is core-local state, it does not change what any *other* core observes,
// and this core takes traps only at instruction boundaries, so it already sees its own accesses in
// program order. An `mfence` under every spinlock acquire and release is a real cost for a
// guarantee nothing needs.
#[inline(always)]
pub fn disable() -> bool {
    let state = crate::arch::interrupt::disable();
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    state
}

/// Set the current interrupt enable state.
#[inline(always)]
pub fn set(state: bool) {
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    crate::arch::interrupt::set(state);
}

/// Get the current interrupt enable state without modifying it.
#[inline(always)]
pub fn get() -> bool {
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    crate::arch::interrupt::get()
}

#[inline]
#[track_caller]
pub fn with_disabled<T, F: FnOnce() -> T>(f: F) -> T {
    let tmp = disable();
    let t = f();
    set(tmp);
    t
}

#[inline]
pub fn post_interrupt() {
    schedule_maybe_preempt();
}

/// Interrupts taken since boot, as a single relaxed counter. Exported to userspace
/// (`KernelStats::interrupts`) and read as a rate.
static TAKEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn taken() -> u64 {
    TAKEN.load(Ordering::Relaxed)
}

/// Count one hardware interrupt. Call sites pass only real interrupts: a cpu exception is not one,
/// and page faults are counted on their own path.
#[inline]
pub fn count_interrupt() {
    TAKEN.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn send_ipi(destination: Destination, ipi: InterProcessorInterrupt) {
    arch::send_ipi(destination, ipi as u32)
}

#[derive(Debug, Clone, Copy)]
pub enum PinPolarity {
    ActiveHigh,
    ActiveLow,
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerMode {
    Edge,
    Level,
}

#[derive(Debug, Clone, Copy)]
pub enum Destination {
    Bsp,
    Single(u32),
    LowestPriority,
    AllButSelf,
    All,
}

pub struct WakeInfo {
    obj: ObjectRef,
    offset: usize,
}

impl WakeInfo {
    pub fn wake(&self, val: u64) {
        let _ = self
            .obj
            .try_write_val_and_signal(self.offset, val, usize::MAX)
            .inspect_err(|e| log::error!("failed to raise interrupt: {}", e));
    }

    pub fn new(obj: ObjectRef, offset: usize) -> Self {
        Self { obj, offset }
    }
}

struct InterruptInner {
    target: Vec<WakeInfo>,
}

impl InterruptInner {
    pub fn raise(&self, val: u64) {
        for wi in &self.target {
            wi.wake(val)
        }
    }
}
struct Interrupt {
    inner: Spinlock<InterruptInner>,
    vector: usize,
}

impl Interrupt {
    pub fn raise(&self) {
        self.inner.lock().raise(self.vector as u64);
    }

    fn add(&self, wi: WakeInfo) {
        self.inner.lock().target.push(wi)
    }

    fn new(vector: usize) -> Self {
        Self {
            inner: Spinlock::new(InterruptInner { target: Vec::new() }),
            vector,
        }
    }
}

struct DeviceInterrupter {
    word_object: KernelObjectVirtHandle<DeviceRepr>,
    raw_word: *const AtomicU64,
}

unsafe impl Send for DeviceInterrupter {}
unsafe impl Sync for DeviceInterrupter {}

impl DeviceInterrupter {
    fn new(wi: &WakeInfo) -> Self {
        let word_object = kernel_context().insert_kernel_object(ObjectContextInfo::new(
            wi.obj.clone(),
            Protections::WRITE | Protections::READ,
            CacheType::WriteBack,
            MapFlags::empty(),
        ));
        let raw_word =
            word_object.lea_raw(wi.offset as *const AtomicU64).unwrap() as *const AtomicU64;
        (unsafe { &*raw_word }).store(0, Ordering::Release);
        Self {
            word_object,
            raw_word,
        }
    }
}

const MAX_DEVICE_VECTORS: usize = 16;

/// Waiters claimed per pass of the device-interrupt wake in [external_interrupt_entry]. Matches
/// `obj::thread_sync`'s batch, and for the same reason: a device word rarely has more than one or
/// two waiters, and those take a single pass.
const WAKE_BATCH: usize = 16;

type DeviceVectors =
    [Spinlock<heapless::Vec<DeviceInterrupter, MAX_DEVICE_VECTORS>>; MAX_VECTOR + 1];
type DeviceWaiters = [Spinlock<RBTree<ThreadSleepAdapter>>; MAX_VECTOR + 1];

/// Statics rather than fields built in `get_global_interrupts`: the closure put both arrays on the
/// calling thread's stack, over 256 KiB, before moving them into the `Once`.
static DEVICE_VECTORS: DeviceVectors =
    [const { Spinlock::new(heapless::Vec::new()) }; MAX_VECTOR + 1];
static DEVICE_WAITERS: DeviceWaiters =
    [const { Spinlock::new(RBTree::new(ThreadSleepAdapter::NEW)) }; MAX_VECTOR + 1];

struct GlobalInterruptState {
    ints: Vec<Interrupt>,
    device_vectors: &'static DeviceVectors,
    device_waiters: &'static DeviceWaiters,
}

impl GlobalInterruptState {
    fn setup_device_wait(
        &self,
        thread: ThreadRef,
        vector: u32,
        ptr: *const AtomicU64,
        set_sync_sleep: bool,
    ) -> bool {
        let word = unsafe { ptr.as_ref_unchecked() };
        log::trace!(
            "thread {} in device wait vector {} (ptr = {:p}, val = {})",
            thread.id(),
            vector,
            ptr,
            word.load(Ordering::Relaxed)
        );
        if word.load(Ordering::Relaxed) != 0 {
            return false;
        }
        let mut waiters = self.device_waiters[vector as usize].lock();
        if word.load(Ordering::SeqCst) != 0 {
            return false;
        }
        if set_sync_sleep {
            thread.set_sync_sleep();
        }
        // Mirrors `SleepEntry::add_thread`. This guard was tried once on its own and reverted,
        // because back then `external_interrupt_entry` requeued this tree unconditionally and
        // claimed through `reset_sync_sleep_done` rather than `reset_sync_sleep`: the entry it left
        // behind fired a spurious wake at whatever that thread slept on next. That half is fixed
        // now -- the wake below claims per waiter and removes under this lock -- so a leftover
        // entry can no longer wake anyone, and stopping the duplicate is what keeps
        // `remove_from_device_wait`'s single find-and-remove sufficient.
        if !waiters.find(&thread.objid()).is_null() {
            return true;
        }
        waiters.insert(thread);
        true
    }

    /// Claim and requeue every waiter on `number`. With `unlink_claimed`, waiters someone else
    /// already claimed are unlinked too rather than left for their own `remove_from_device_wait`,
    /// which looks the vector up in a device table that a free has already cleared.
    fn wake_device_waiters(&self, number: u32, unlink_claimed: bool) {
        // Claim-then-remove per waiter, under the lock, exactly as `SleepEntry::claim_n` does.
        //
        // `take()` used to detach the whole tree and drain it after dropping the lock. Its nodes
        // stayed linked while unreachable from `device_waiters`, so a concurrent
        // `remove_from_device_wait` found nothing and silently did nothing -- the waiter then
        // finished its round believing it was unlinked, `reset` zeroed its slot counter, and the
        // next round handed out a slot whose link was still in the detached tree. Two trees, one
        // link. Removing here means a node is either in this tree and removable, or gone and
        // claimed, with no window in between.
        //
        // Claiming through `reset_sync_sleep` rather than `add_all_to_requeue`'s
        // `reset_sync_sleep_done` matches the flag `setup_device_wait` arms above, so a waiter that
        // someone else already claimed is left alone instead of woken twice.
        //
        // Claim under the lock, requeue outside it, exactly as `Object::wakeup_word` does. This
        // runs in hard interrupt context, and `add_to_requeue`'s fast path is `schedule_thread` --
        // a topology walk, a remote run queue lock and a wakeup IPI -- so per waiter under this
        // spinlock it was all inside the interrupt, bounded only by the waiter count. Deferring it
        // costs nothing here: `schedule_thread` does not switch from an interrupt, it inserts and
        // either marks preempt or signals, and `post_interrupt` consumes the mark on the way out.
        //
        // Critical across the drain for the reason `Object::wakeup_word` gives: a claimed waiter
        // sits in `batch` on this stack over a window the spinlock used to cover, and this thread
        // exiting at a poll point in that window would take the stack and the wakeups with it.
        let _critical = current_thread_ref().map(|ct| ct.enter_critical());
        loop {
            let mut batch = heapless::Vec::<ThreadRef, WAKE_BATCH>::new();
            // Refs unlinked without a claim, dropped once the lock is released.
            let mut unlinked = heapless::Vec::<ThreadRef, WAKE_BATCH>::new();
            {
                let mut waiters = self.device_waiters[number as usize].lock();
                let mut cursor = waiters.front_mut();
                while !batch.is_full() && !unlinked.is_full() && !cursor.is_null() {
                    if cursor.get().is_some_and(|t| t.reset_sync_sleep()) {
                        let thread = cursor.remove().unwrap();
                        // Safety: not full, checked above.
                        unsafe { batch.push_unchecked(thread) };
                    } else if unlink_claimed {
                        let thread = cursor.remove().unwrap();
                        // Safety: not full, checked above.
                        unsafe { unlinked.push_unchecked(thread) };
                    } else {
                        cursor.move_next();
                    }
                }
            }
            // Entries skipped for being already claimed stay in the tree and are re-walked, so a
            // full batch says only that we ran out of room -- terminate on the empty one.
            let full = batch.is_full() || unlinked.is_full();
            let unlinked_any = !unlinked.is_empty();
            drop(unlinked);
            if batch.is_empty() && !unlinked_any {
                break;
            }
            for thread in batch {
                if !unlink_claimed {
                    thread
                        .sched
                        .set_irq_hint(crate::processor::mp::current_processor().id);
                }
                add_to_requeue(thread);
            }
            if !full {
                break;
            }
        }
        requeue_all();
    }
}

static GLOBAL_INT: Once<GlobalInterruptState> = Once::new();
fn get_global_interrupts() -> &'static GlobalInterruptState {
    GLOBAL_INT.call_once(|| {
        let mut v = Vec::new();
        for i in 0..NUM_VECTORS {
            v.push(Interrupt::new(i));
        }
        GlobalInterruptState {
            ints: v,
            device_vectors: &DEVICE_VECTORS,
            device_waiters: &DEVICE_WAITERS,
        }
    })
}

pub fn set_userspace_interrupt_wakeup(number: u32, wi: WakeInfo) {
    let gi = get_global_interrupts();
    let di = DeviceInterrupter::new(&wi);
    let mut vectors = gi.device_vectors[number as usize].lock();
    if !vectors.is_full() {
        let _ = vectors.push(di);
    } else {
        drop(vectors);
        log::warn!("trying to setup too many device interrupt wakers, overflowing...");
        gi.ints[number as usize].add(wi);
    }
}

pub fn handle_interrupt(number: u32) {
    let gi = get_global_interrupts();
    gi.ints[number as usize].raise();
}

pub fn wait_for_device_interrupt(
    thread: &ThreadRef,
    number: u32,
    first_wait: bool,
    ptr: *const AtomicU64,
) -> bool {
    let gi = get_global_interrupts();
    let res = gi.setup_device_wait(thread.clone(), number, ptr, first_wait);
    return res;
}

pub fn remove_from_device_wait(thread: &ThreadRef, number: u32) {
    let gi = get_global_interrupts();
    let mut waiters = gi.device_waiters[number as usize].lock();
    waiters.find_mut(&thread.objid()).remove();
}

const INTQUEUE_LEN: usize = 128;
struct InterruptQueue {
    queue: [u32; INTQUEUE_LEN],
    head: usize,
    tail: usize,
}

impl InterruptQueue {
    const fn new() -> Self {
        Self {
            queue: [0; INTQUEUE_LEN],
            head: 0,
            tail: 0,
        }
    }

    fn is_full(&self) -> bool {
        (self.tail + 1) % INTQUEUE_LEN == self.head
    }

    fn enqueue(&mut self, int: u32) {
        if self.is_full() {
            // TODO: extend this mechanism to avoid dropping interrupts
            return;
        }
        self.queue[self.head] = int;
        self.head = (self.head + 1) % INTQUEUE_LEN;
    }

    fn dequeue(&mut self) -> Option<u32> {
        if self.tail == self.head {
            None
        } else {
            let ret = self.queue[self.tail];
            self.tail = (self.tail + 1) % INTQUEUE_LEN;
            Some(ret)
        }
    }
}

static INT_QUEUE: Spinlock<InterruptQueue> = Spinlock::new(InterruptQueue::new());
static INT_THREAD: Once<ThreadRef> = Once::new();
static INT_THREAD_CONDVAR: CondVar = CondVar::new();

extern "C" fn soft_interrupt_waker() {
    /* TODO: use some heuristic to decide if we need to spend more time handling timeouts */
    loop {
        let mut iq = INT_QUEUE.lock();
        let mut ints = [0; INTQUEUE_LEN];
        let mut count = 0;
        while let Some(int) = iq.dequeue() {
            ints[count] = int;
            count += 1;
        }

        if count > 0 {
            drop(iq);
            for i in 0..count {
                handle_interrupt(ints[i]);
            }
        } else {
            let _ = INT_THREAD_CONDVAR.wait(iq);
        }
    }
}

pub fn init() {
    INT_THREAD.call_once(|| {
        // TODO: priority?
        crate::thread::entry::start_new_kernel(
            Priority::INTERRUPT,
            soft_interrupt_waker,
            0,
            "soft-int-waker",
        )
    });
}

pub fn external_interrupt_entry(number: u32) {
    routestats::note(number);
    let gi = get_global_interrupts();
    let vectors = gi.device_vectors[number as usize].lock();
    if vectors.is_empty() {
        drop(vectors);
        soft_raise(number);
        return;
    }
    for di in vectors.iter() {
        unsafe {
            di.raw_word
                .as_ref_unchecked()
                .store(number as u64, Ordering::Release)
        };
    }
    // A full table may have spilled further wakers into `ints`, which only the soft path raises.
    let overflowed = vectors.is_full();
    drop(vectors);
    gi.wake_device_waiters(number, false);
    if overflowed {
        soft_raise(number);
    }
}

fn soft_raise(number: u32) {
    let mut iq = INT_QUEUE.lock();
    iq.enqueue(number);
    INT_THREAD_CONDVAR.signal();
}

/// Undo every [set_userspace_interrupt_wakeup] on `number` and release its waiters, ahead of the
/// vector being freed. It can be handed to another device next, so nothing may stay linked on it.
pub fn clear_userspace_interrupt_wakeup(number: u32) {
    let gi = get_global_interrupts();
    // Taken out under the locks, dropped outside them: both hold object references.
    let interrupters = core::mem::take(&mut *gi.device_vectors[number as usize].lock());
    let targets = core::mem::take(&mut gi.ints[number as usize].inner.lock().target);
    drop((interrupters, targets));
    gi.wake_device_waiters(number, true);
}

/// Which cpus each external vector was taken on, to check that a routed interrupt lands where
/// it was sent. Printed at shutdown.
pub mod routestats {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::{arch::interrupt::NUM_VECTORS, processor::mp::current_processor};

    static COUNT: [AtomicU64; NUM_VECTORS] = [const { AtomicU64::new(0) }; NUM_VECTORS];
    /// Device-interrupt wakes placed, how many landed on the interrupt's cpu, and how many of
    /// those the irq-affine rule chose.
    static DEV_WAKES: AtomicU64 = AtomicU64::new(0);
    static DEV_WAKES_LOCAL: AtomicU64 = AtomicU64::new(0);
    static DEV_WAKES_RULE: AtomicU64 = AtomicU64::new(0);

    pub fn note_device_wake(local: bool, by_rule: bool) {
        DEV_WAKES.fetch_add(1, Ordering::Relaxed);
        if local {
            DEV_WAKES_LOCAL.fetch_add(1, Ordering::Relaxed);
        }
        if by_rule {
            DEV_WAKES_RULE.fetch_add(1, Ordering::Relaxed);
        }
    }
    /// Bit `n` for cpu id `n`; ids past 63 fold onto bit 63.
    static CPUS: [AtomicU64; NUM_VECTORS] = [const { AtomicU64::new(0) }; NUM_VECTORS];

    pub(super) fn note(vector: u32) {
        let v = vector as usize;
        COUNT[v].fetch_add(1, Ordering::Relaxed);
        CPUS[v].fetch_or(1 << current_processor().id.min(63), Ordering::Relaxed);
    }

    pub fn print() {
        emerglogln!(
            "irqwake: device wakes {} on irq cpu {} by rule {} (irq_affine {})",
            DEV_WAKES.load(Ordering::Relaxed),
            DEV_WAKES_LOCAL.load(Ordering::Relaxed),
            DEV_WAKES_RULE.load(Ordering::Relaxed),
            crate::irq_affine()
        );
        for v in 0..NUM_VECTORS {
            let n = COUNT[v].load(Ordering::Relaxed);
            if n != 0 {
                emerglogln!(
                    "irqroute: vector {} n={} cpus={:#x}",
                    v,
                    n,
                    CPUS[v].load(Ordering::Relaxed)
                );
            }
        }
    }
}

#[derive(Debug)]
pub struct DynamicInterrupt {
    vec: usize,
}

pub fn allocate_interrupt(
    pri: InterruptPriority,
    opts: InterruptAllocateOptions,
    destination: Destination,
) -> Option<DynamicInterrupt> {
    crate::arch::interrupt::allocate_interrupt_vector(pri, opts, destination)
}

impl DynamicInterrupt {
    pub fn new(vec: usize) -> Self {
        Self { vec }
    }

    pub fn num(&self) -> usize {
        self.vec
    }
}
