use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use intrusive_collections::LinkedList;
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
        kernel_context, virtmem::KernelObjectVirtHandle, KernelMemoryContext, KernelObjectHandle,
        ObjectContextInfo,
    },
    mutex::MutexLinkAdapter,
    obj::ObjectRef,
    once::Once,
    processor::sched::schedule_maybe_preempt,
    spinlock::Spinlock,
    syscall::sync::{add_all_to_requeue, requeue_all},
    thread::{priority::Priority, ThreadRef},
};

/// Set the current interrupt enable state to disabled and return the old state.
#[inline]
pub fn disable() -> bool {
    let state = crate::arch::interrupt::disable();
    core::sync::atomic::fence(Ordering::SeqCst);
    state
}

/// Set the current interrupt enable state.
#[inline]
pub fn set(state: bool) {
    core::sync::atomic::fence(Ordering::SeqCst);
    crate::arch::interrupt::set(state);
}

/// Get the current interrupt enable state without modifying it.
#[inline]
pub fn get() -> bool {
    core::sync::atomic::fence(Ordering::SeqCst);
    crate::arch::interrupt::get()
}

#[inline]
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
        //logln!("wake! {}", val);
        unsafe {
            self.obj
                .try_write_val_and_signal(self.offset, val, usize::MAX);
        }
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

struct GlobalInterruptState {
    ints: Vec<Interrupt>,
    device_vectors:
        [Spinlock<heapless::Vec<DeviceInterrupter, MAX_DEVICE_VECTORS>>; MAX_VECTOR + 1],
    device_waiters: [Spinlock<LinkedList<MutexLinkAdapter>>; MAX_VECTOR + 1],
}

impl GlobalInterruptState {
    fn setup_device_wait(&self, thread: ThreadRef, vector: u32, ptr: *const AtomicU64) -> bool {
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
        waiters.push_back(thread);
        true
    }
}

static GLOBAL_INT: Once<GlobalInterruptState> = Once::new();
fn get_global_interrupts() -> &'static GlobalInterruptState {
    let mut v = Vec::new();
    for i in 0..NUM_VECTORS {
        v.push(Interrupt::new(i));
    }
    GLOBAL_INT.call_once(|| GlobalInterruptState {
        ints: v,
        device_vectors: [const { Spinlock::new(heapless::Vec::new()) }; MAX_VECTOR + 1],
        device_waiters: [const { Spinlock::new(LinkedList::new(MutexLinkAdapter::NEW)) };
            MAX_VECTOR + 1],
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
    let res = gi.setup_device_wait(thread.clone(), number, ptr);
    if first_wait && res {
        thread.set_sync_sleep();
    }
    return res;
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
            INT_THREAD_CONDVAR.wait(iq);
        }
    }
}

pub fn init() {
    INT_THREAD.call_once(|| {
        // TODO: priority?
        crate::thread::entry::start_new_kernel(Priority::INTERRUPT, soft_interrupt_waker, 0)
    });
}

pub fn external_interrupt_entry(number: u32) {
    let gi = get_global_interrupts();
    let vectors = gi.device_vectors[number as usize].lock();
    if !vectors.is_empty() && !vectors.is_full() {
        for di in vectors.iter() {
            log::trace!(
                "got external interrupt {}, storing to word {:p}",
                number,
                di.raw_word
            );
            unsafe {
                di.raw_word
                    .as_ref_unchecked()
                    .store(number as u64, Ordering::Release)
            };
        }
        drop(vectors);
        let mut waiters = gi.device_waiters[number as usize].lock();
        let list = waiters.take();
        drop(waiters);
        add_all_to_requeue(list);
        requeue_all();
        return;
    }
    let mut iq = INT_QUEUE.lock();
    iq.enqueue(number);
    INT_THREAD_CONDVAR.signal();
}

#[derive(Debug)]
pub struct DynamicInterrupt {
    vec: usize,
}

pub fn allocate_interrupt(
    pri: InterruptPriority,
    opts: InterruptAllocateOptions,
) -> Option<DynamicInterrupt> {
    crate::arch::interrupt::allocate_interrupt_vector(pri, opts)
}

impl DynamicInterrupt {
    pub fn new(vec: usize) -> Self {
        Self { vec }
    }

    pub fn num(&self) -> usize {
        self.vec
    }
}
