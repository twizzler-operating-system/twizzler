use core::{
    alloc::Layout,
    cell::RefCell,
    sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering},
};

use alloc::{boxed::Box, sync::Arc};
use intrusive_collections::{linked_list::AtomicLink, offset_of, RBTreeAtomicLink};
use twizzler_abi::{
    object::{ObjID, NULLPAGE_SIZE},
    syscall::ThreadSpawnArgs,
    thread::{ExecutionState, ThreadRepr},
    upcall::UpcallInfo,
};

use crate::{
    idcounter::{Id, IdCounter},
    interrupt,
    memory::context::{ContextRef, UserContext},
    obj::control::ControlObjectCacher,
    processor::{get_processor, KERNEL_STACK_SIZE},
    spinlock::Spinlock,
};

use self::{
    flags::{THREAD_IN_KERNEL, THREAD_PROC_IDLE},
    priority::{Priority, PriorityClass},
};

pub mod entry;
mod flags;
pub mod priority;
pub mod state;
pub mod suspend;

pub use flags::{enter_kernel, exit_kernel};

#[derive(Debug, Default)]
pub struct ThreadStats {
    pub user: AtomicU64,
    pub sys: AtomicU64,
    pub idle: AtomicU64,
    pub last: AtomicU64,
}

pub struct Thread {
    pub arch: crate::arch::thread::ArchThread,
    pub priority: Priority,
    pub flags: AtomicU32,
    pub last_cpu: AtomicI32,
    pub affinity: AtomicI32,
    pub critical_counter: AtomicU64,
    id: Id<'static>,
    pub switch_lock: AtomicU64,
    pub donated_priority: Spinlock<Option<Priority>>,
    pub current_processor_queue: AtomicI32,
    memory_context: Option<ContextRef>,
    pub kernel_stack: Box<[u8; KERNEL_STACK_SIZE]>,
    pub stats: ThreadStats,
    spawn_args: Option<ThreadSpawnArgs>,
    pub control_object: ControlObjectCacher<ThreadRepr>,
    // TODO: consider reusing one of these for the others.
    pub sched_link: AtomicLink,
    pub mutex_link: AtomicLink,
    pub condvar_link: RBTreeAtomicLink,
    pub suspend_link: RBTreeAtomicLink,
}
unsafe impl Send for Thread {}

pub type ThreadRef = Arc<Thread>;

#[thread_local]
static CURRENT_THREAD: RefCell<Option<ThreadRef>> = RefCell::new(None);

pub fn current_thread_ref() -> Option<ThreadRef> {
    if core::intrinsics::unlikely(!crate::processor::tls_ready()) {
        return None;
    }
    interrupt::with_disabled(|| CURRENT_THREAD.borrow().clone())
}

pub fn set_current_thread(thread: ThreadRef) {
    interrupt::with_disabled(move || {
        let old = CURRENT_THREAD.replace(Some(thread));
        drop(old);
    });
}

static ID_COUNTER: IdCounter = IdCounter::new();

pub fn current_memory_context() -> Option<ContextRef> {
    current_thread_ref()
        .map(|t| t.memory_context.clone())
        .flatten()
}

impl Thread {
    pub fn new(
        ctx: Option<ContextRef>,
        spawn_args: Option<ThreadSpawnArgs>,
        priority: Priority,
    ) -> Self {
        /* TODO: dedicated kernel stack allocator, with guard page support */
        let kernel_stack = unsafe {
            let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
            alloc::alloc::alloc_zeroed(layout)
        };
        Self {
            arch: crate::arch::thread::ArchThread::new(),
            priority,
            id: ID_COUNTER.next(),
            flags: AtomicU32::new(THREAD_IN_KERNEL),
            kernel_stack: unsafe { Box::from_raw(core::intrinsics::transmute(kernel_stack)) },
            critical_counter: AtomicU64::new(0),
            switch_lock: AtomicU64::new(0),
            affinity: AtomicI32::new(-1),
            last_cpu: AtomicI32::new(-1),
            donated_priority: Spinlock::new(None),
            current_processor_queue: AtomicI32::new(-1),
            stats: ThreadStats::default(),
            memory_context: ctx,
            spawn_args,
            control_object: ControlObjectCacher::new(ThreadRepr::default()),
            sched_link: AtomicLink::default(),
            mutex_link: AtomicLink::default(),
            suspend_link: RBTreeAtomicLink::default(),
            condvar_link: RBTreeAtomicLink::default(),
        }
    }

    pub fn new_idle() -> Self {
        let mut thread = Self::new(None, None, Priority::default_idle());
        thread.flags.fetch_or(THREAD_PROC_IDLE, Ordering::SeqCst);
        thread.priority.class = PriorityClass::Idle;
        thread.switch_lock.store(1, Ordering::SeqCst);
        thread
    }

    pub fn objid(&self) -> ObjID {
        self.control_object.object().id()
    }

    pub fn switch_thread(&self, current: &Thread) {
        if self != current {
            if let Some(ref ctx) = self.memory_context {
                ctx.switch_to();
            }
        }
        self.arch_switch_to(current)
    }

    pub fn do_critical<F, T>(&self, mut f: F) -> T
    where
        F: FnMut(&Self) -> T,
    {
        self.critical_counter.fetch_add(1, Ordering::SeqCst);
        let res = f(self);
        self.critical_counter.fetch_sub(1, Ordering::SeqCst);
        res
    }

    #[inline]
    pub fn is_critical(&self) -> bool {
        self.critical_counter.load(Ordering::SeqCst) > 0
    }

    #[inline]
    pub fn exit_critical(&self) {
        let res = self.critical_counter.fetch_sub(1, Ordering::SeqCst);
        assert!(res > 0);
    }

    #[inline]
    pub fn enter_critical(&self) -> CriticalGuard {
        self.critical_counter.fetch_add(1, Ordering::SeqCst);
        CriticalGuard { thread: self }
    }

    #[inline]
    pub fn enter_critical_unguarded(&self) {
        self.critical_counter.fetch_add(1, Ordering::SeqCst);
    }

    pub fn maybe_reschedule_thread(&self) {
        let ccpu = self.current_processor_queue.load(Ordering::SeqCst);
        /* if we get -1 here, the thread is either running or blocked, not waiting on a queue. There's a small race condition, here, though,
        since we check this variable and then lock a scheduler queue. It's possible that the thread was placed on a queue, then this variable was set,
        and then we load it, and then the thread is run. This results in a spurious reschedule. It's probably rare, though, but we should profile this
        to see if it's a problem.

        Another possible race condition is the opposite: a thread is running, and we read -1, and then it gets put on the queue. This is also probably
        okay, since that means that we might not have really needed to do a reschedule.

        Finally, note that this function should be called with the donated_priority lock held, since that will force serialization by any schedulers
        calculating the thread's priority at the time of this call. Or, if the HAS_DONATED_PRIORITY flag is clear, it will not, but that is okay too.
        But this does mean we need to submit any wakeups/reschedules with interrupts cleared. */
        //TODO: verify the above logic
        //TODO: optimize this by keeping an is_running flag?
        if ccpu == -1 {
            return;
        }
        let ccpu = ccpu as u32;
        let proc = get_processor(ccpu);
        let resched = proc.schedlock().check_priority_change(self);
        if resched {
            interrupt::with_disabled(|| proc.wakeup(true));
        }
    }

    /// Set the state of the thread. This publishes thread info to userspace.
    /// This function may be called in a critical section only if:
    ///   1: transitioning between running and sleeping
    ///   2: state == current state
    pub fn set_state_and_code(&self, state: ExecutionState, code: u64) {
        if (state == ExecutionState::Exited || state == ExecutionState::Suspended)
            && self.is_current_thread()
            && self.is_critical()
        {
            panic!("cannot signal wake up in set_state_and_code due to call from critical section");
        }
        let base = self.control_object.base();
        let old_state = base.set_state(state, code);

        // Note that since this value can be written to by userspace, we must check if we're
        // critical because we can't rely on userspace following the rules. Same for checking if
        // the state is changing.
        if !(old_state == ExecutionState::Running && state == ExecutionState::Sleeping
            || old_state == ExecutionState::Sleeping && state == ExecutionState::Running)
            && (old_state != state
                || state == ExecutionState::Exited
                || state == ExecutionState::Suspended)
            && !current_thread_ref().map_or(true, |ct| ct.is_critical())
            && old_state != ExecutionState::Exited
        {
            self.control_object
                .object()
                .wakeup_word(NULLPAGE_SIZE + offset_of!(ThreadRepr, status), usize::MAX);
        }
    }

    pub fn is_current_thread(&self) -> bool {
        if let Some(cur) = current_thread_ref() {
            self.objid() == cur.objid()
        } else {
            false
        }
    }

    #[inline]
    /// See set_state_and_code for details.
    pub fn set_state(&self, state: ExecutionState) {
        self.set_state_and_code(state, 0)
    }

    pub fn get_state(&self) -> ExecutionState {
        let base = self.control_object.base();
        base.get_state()
    }

    #[inline]
    pub fn id(&self) -> u64 {
        self.id.value()
    }

    pub fn send_upcall(&self, info: UpcallInfo) {
        // TODO
        let ctx = current_memory_context().unwrap();
        let upcall = ctx.get_upcall().unwrap();
        self.arch_queue_upcall(upcall, info);
    }
}

impl Eq for Thread {}

impl PartialEq for Thread {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl PartialOrd for Thread {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Thread {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

pub struct CriticalGuard<'a> {
    thread: &'a Thread,
}

impl<'a> Drop for CriticalGuard<'a> {
    fn drop(&mut self) {
        self.thread.exit_critical();
    }
}

pub fn exit(code: u64) -> ! {
    {
        let th = current_thread_ref().unwrap();
        th.set_state_and_code(ExecutionState::Exited, code);
        crate::interrupt::disable();
        th.set_is_exiting();
        crate::syscall::sync::remove_from_requeue(&th);
        crate::sched::remove_thread(th.id());
        drop(th);
    }
    crate::sched::schedule(false);
    unreachable!()
}
