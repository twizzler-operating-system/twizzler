use alloc::{boxed::Box, sync::Arc};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    fmt::Debug,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    u32,
};

use intrusive_collections::{RBTreeAtomicLink, linked_list::AtomicLink, offset_of};
use time::{SAMPLE_PERIOD_TICKS, ThreadSched, ThreadStats};
use twizzler_abi::{
    object::{NULLPAGE_SIZE, ObjID},
    syscall::{PERTHREAD_TRACE_GEN_SAMPLE, ThreadSpawnArgs},
    thread::{ExecutionState, ThreadRepr},
    trace::{ThreadSamplingEvent, TraceEntryFlags, TraceKind},
    upcall::{UPCALL_EXIT_CODE, UpcallFlags, UpcallInfo, UpcallMode, UpcallTarget},
};
use twizzler_rt_abi::error::TwzError;

use self::{flags::THREAD_PROC_IDLE, priority::Priority};
use crate::{
    idcounter::{Id, IdCounter},
    interrupt::Destination,
    memory::{
        VirtAddr,
        context::{
            ContextRef, UserContext,
            virtmem::{Slot, VirtContext},
        },
    },
    obj::{ThreadSleepLinker, control::ControlObjectCacher},
    processor::{
        KERNEL_STACK_SIZE,
        ipi::ipi_exec,
        mp::get_processor,
        sched::{SchedFlags, remove_thread, schedule, schedule_resched, with_all_threads},
    },
    security::SecCtxMgr,
    spinlock::Spinlock,
    thread::{
        flags::THREAD_MUST_EXIT,
        locktrack::{LockTracker, deregister_lock_tracker, register_lock_tracker},
    },
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

pub mod entry;
mod flags;
pub mod locktrack;
pub mod priority;
pub mod suspend;
pub mod time;

pub use flags::{enter_kernel, exit_kernel};

pub struct Thread {
    pub arch: crate::arch::thread::ArchThread,
    // TODO: determine how to order and pad these to minimize false sharing.
    pub priority: AtomicU32,
    pub stable_priority: AtomicU32,
    pub flags: AtomicU32,
    pub sched: ThreadSched,
    pub critical_counter: AtomicU64,
    /// Caller that took `critical_counter` from 0 to 1, as a `&'static Location` pointer (0 =
    /// none). Diagnostic only: Mode C surfaces at whichever mutex a thread with a leaked count
    /// happens to take next, which never names the entry that leaked it. This does.
    critical_origin: AtomicUsize,
    id: Id<'static>,
    pub switch_lock: AtomicU64,
    /// Set the first time this thread is switched to. Until then it has never executed, so it
    /// cannot be any cpu's current thread, which is what makes early publication safe for it.
    has_run: AtomicBool,
    /// Bumped every time a sync sleep ends. A timeout callback captures this when it is registered
    /// and does nothing if it no longer matches, which is what makes a callback that is already
    /// past `soft_advance` -- and so beyond the reach of `TimeoutKey::release` -- harmless.
    sync_sleep_gen: AtomicU64,
    pub donated_priority: AtomicU32,
    memory_context: Option<ContextRef>,
    pub kernel_stack: Box<[u8; KERNEL_STACK_SIZE]>,
    pub stats: ThreadStats,
    spawn_args: Option<ThreadSpawnArgs>,
    pub control_object: ControlObjectCacher<ThreadRepr>,
    pub upcall_target: Spinlock<Option<UpcallTarget>>,
    pub sched_link: AtomicLink,
    pub mutex_link: AtomicLink,
    pub memwait_link: AtomicLink,
    pub pager_link: AtomicLink,
    pub condvar_link: RBTreeAtomicLink,
    pub requeue_link: RBTreeAtomicLink,
    pub suspend_link: RBTreeAtomicLink,
    pub sync_links: ThreadSleepLinker,
    pub secctx: SecCtxMgr,
    pub sample_expire: Spinlock<Option<u64>>,
    pub self_reference: UnsafeCell<*mut ThreadRef>,
    pub pending_message: AtomicU64,
    /// Upcalls generated since the thread last returned to userspace. See `send_upcall`.
    pub upcalls_since_user: AtomicU32,
    pub last_pf_addr: AtomicU64,
    pub last_pf_kind: AtomicU32,
    pub last_pf_flags: AtomicU32,
    mutex_count: AtomicU32,
    /// Consecutive orphan scans that found this thread on no queue. Written only by the scanning
    /// cpu. See `check_orphan_threads`.
    offqueue_scans: AtomicU32,
    /// Depth of nested kernel entries (syscall, fault, exception). Zero means the thread is
    /// executing in userspace. A counter rather than a flag because a fault taken while already
    /// in the kernel must not report a return to user when only the inner handler finishes.
    kernel_depth: AtomicU32,
    lock_tracker: Arc<LockTracker>,
    lock_tracker_index: Option<usize>,
}
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

pub type ThreadRef = Arc<Thread>;

impl Debug for Thread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Thread")
            .field("id", &self.id)
            .field("objid", &self.objid())
            .finish()
    }
}

#[thread_local]
static CURRENT_THREAD: UnsafeCell<*const ThreadRef> = UnsafeCell::new(core::ptr::null());

/// Offset of `CURRENT_THREAD` from the thread pointer, computed once and identical on every cpu
/// (one ELF TLS template, so one layout). Zero means "not computed yet"; no TLS variable sits at
/// the thread pointer itself under either variant.
static CURRENT_THREAD_TPOFF: AtomicUsize = AtomicUsize::new(0);

/// Compute and cache the offset. Interrupts are off because the two halves -- the variable's
/// address and this cpu's thread pointer -- must come from the *same* cpu, which is the very
/// property `read_current_thread_ptr` exists to guarantee.
#[cfg(target_arch = "x86_64")]
#[cold]
fn init_current_thread_tpoff() -> usize {
    let int = crate::interrupt::disable();
    let off = (CURRENT_THREAD.get() as usize).wrapping_sub(crate::arch::processor::tls_base());
    CURRENT_THREAD_TPOFF.store(off, Ordering::Relaxed);
    crate::interrupt::set(int);
    off
}

/// Read this cpu's current-thread pointer in a single instruction.
///
/// The obvious `*CURRENT_THREAD.get()` is **not** safe against preemption, and this was the defect
/// behind Mode C and its two sibling mutex panics. Taking the address of a `#[thread_local]` makes
/// the compiler materialize `thread_pointer + offset` into a general register -- in a debug build
/// it then spills that register to the stack and dereferences it several calls later. A general
/// register survives migration; the thread pointer does not. So a thread preempted inside that
/// window and resumed on another cpu completes the load against the *previous* cpu's TLS block and
/// gets whatever thread that cpu has since picked up. Everything downstream is then charged to a
/// thread running somewhere else: a critical count (Mode C), a mutex's recorded owner, a wait-list
/// membership.
///
/// A segment-relative load has no such window: the base comes from the segment register, which is
/// cpu state and is correct wherever the instruction retires, and an instruction is indivisible
/// with respect to interrupts. Disabling interrupts around the Rust version would also work, but it
/// costs far more on the kernel's hottest path and it silently depends on the interrupt asm
/// carrying a memory clobber strong enough to stop the load moving across it.
// The x86 body is one segment-relative load, and this sits on the path the comment above calls the
// kernel's hottest. `#[inline]` alone would not survive opt-level 0, where only LLVM's
// always-inliner runs.
#[inline(always)]
fn read_current_thread_ptr() -> *const ThreadRef {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut off = CURRENT_THREAD_TPOFF.load(Ordering::Relaxed);
        if core::intrinsics::unlikely(off == 0) {
            off = init_current_thread_tpoff();
        }
        let p: *const ThreadRef;
        core::arch::asm!(
            "mov {p}, fs:[{off}]",
            p = lateout(reg) p,
            off = in(reg) off,
            options(nostack, readonly, preserves_flags),
        );
        p
    }
    // No segment override to lean on: read the pointer with interrupts off so the thread cannot
    // migrate between materializing the address and loading through it.
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        let int = crate::interrupt::disable();
        let p = *CURRENT_THREAD.get();
        crate::interrupt::set(int);
        p
    }
}

#[inline(always)]
pub fn current_thread_ref() -> Option<&'static ThreadRef> {
    #[allow(unused_unsafe)]
    unsafe {
        if core::intrinsics::unlikely(!crate::processor::tls_ready()) {
            return None;
        }
    }
    core::sync::atomic::fence(Ordering::Acquire);
    unsafe { read_current_thread_ptr().as_ref() }
}

pub unsafe fn set_current_thread(thread: &Thread) {
    locktrack::diag::note_threading_up();
    let ptr = CURRENT_THREAD.get();
    // Raise THREAD_ACTIVE_RUNNING for the incoming thread. Ordinarily its predecessor dropped its
    // own mark in `switch_thread` before the register switch, so the clear below is a no-op; it
    // stays for the paths that reach here without a switch (`create_idle_thread`, and a thread that
    // has never run publishing itself), where this cpu's outgoing thread still carries the mark.
    let old = current_thread_ref();
    if !old.is_some_and(|old| core::ptr::eq(&**old, thread)) {
        if let Some(old) = old {
            old.set_active_running(false);
        }
        // DIAG: the incoming thread is already some cpu's current thread. That is the cross-cpu
        // producer of Mode A's stale-intent/orphan-record pairs: two cpus charging bookkeeping to
        // one thread's tracker, which is also what makes `lock_or_skip` contend and drop records.
        //
        // A cpu handing a thread away drops the mark before `__do_switch` makes it takeable, so
        // this no longer fires for the ordinary deschedule/pick-up handoff -- which it did, at
        // 1-2 per smp2/smp4 run, against an invariant `switch_lock` was holding the whole time.
        //
        // Counted, never printed: this is the middle of a context switch, and `emerglogln` writes
        // the uart synchronously. The count reaches the console via `diag::print_counters`.
        if thread.is_active_running() {
            locktrack::diag::THREAD_CURRENT_ON_TWO_CPUS.count_only();
        }
    }
    thread.set_active_running(true);
    unsafe {
        let r = thread.self_reference.get().as_ref().unwrap_unchecked();
        ptr.write(*r);
    }
    core::sync::atomic::fence(Ordering::Release);
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
        let id = ID_COUNTER.next();
        let lock_tracker = Arc::new(LockTracker::new(id.value()));
        let lock_tracker_index = register_lock_tracker(lock_tracker.clone());
        Self {
            arch: crate::arch::thread::ArchThread::new(),
            priority: AtomicU32::new(priority.raw()),
            stable_priority: AtomicU32::new(priority.raw()),
            id,
            flags: AtomicU32::new(0),
            kernel_stack: unsafe { Box::from_raw(core::intrinsics::transmute(kernel_stack)) },
            critical_counter: AtomicU64::new(0),
            critical_origin: AtomicUsize::new(0),
            switch_lock: AtomicU64::new(0),
            has_run: AtomicBool::new(false),
            sync_sleep_gen: AtomicU64::new(0),
            donated_priority: AtomicU32::new(u32::MAX),
            stats: ThreadStats::new(crate::processor::sched::current_stat_ticks()),
            memory_context: ctx,
            spawn_args,
            control_object: ControlObjectCacher::new(ThreadRepr::default()),
            sched_link: AtomicLink::default(),
            mutex_link: AtomicLink::default(),
            memwait_link: AtomicLink::default(),
            suspend_link: RBTreeAtomicLink::default(),
            requeue_link: RBTreeAtomicLink::default(),
            condvar_link: RBTreeAtomicLink::default(),
            pager_link: AtomicLink::default(),
            sync_links: ThreadSleepLinker::new(),
            upcall_target: Spinlock::new(None),
            secctx: SecCtxMgr::new_kernel(),
            sample_expire: Spinlock::new(None),
            self_reference: UnsafeCell::new(core::ptr::null_mut()),
            sched: ThreadSched::default(),
            pending_message: AtomicU64::new(0),
            upcalls_since_user: AtomicU32::new(0),
            last_pf_addr: AtomicU64::new(0),
            last_pf_kind: AtomicU32::new(0),
            offqueue_scans: AtomicU32::new(0),
            last_pf_flags: AtomicU32::new(0),
            mutex_count: AtomicU32::new(0),
            // Threads start executing in the kernel; jump_to_user() performs the matching exit.
            kernel_depth: AtomicU32::new(1),
            lock_tracker,
            lock_tracker_index,
        }
    }

    pub fn new_idle() -> Self {
        let thread = Self::new(None, None, Priority::IDLE);
        thread.flags.fetch_or(THREAD_PROC_IDLE, Ordering::SeqCst);
        thread.switch_lock.store(1, Ordering::SeqCst);
        thread
    }

    /// Mark this thread as having executed, returning whether it already had. A thread that has
    /// not run yet exists on exactly one run queue and has never been current anywhere.
    pub fn mark_run(&self) -> bool {
        self.has_run.swap(true, Ordering::SeqCst)
    }

    pub fn objid(&self) -> ObjID {
        self.control_object.object().id()
    }

    /// Token identifying the sleep a timeout callback was registered for. Capture it at
    /// registration and re-check it in the callback; see [`Thread::end_sync_sleep`].
    pub fn sync_sleep_gen(&self) -> u64 {
        self.sync_sleep_gen.load(Ordering::SeqCst)
    }

    /// Retire every timeout callback outstanding against this thread's current sleep, by making the
    /// token they captured stale. One atomic store is the whole cancellation: `TimeoutKey::release`
    /// cannot stop a callback that `soft_advance` has already pulled off the queue, and until this
    /// existed such a callback went on to consume the sleep flags and requeue a thread that was by
    /// then running -- which surfaced later as "attempted to insert an object that is already
    /// linked" from the run queue. Call it before releasing the key, so no window is left where the
    /// callback can still see itself as current.
    pub fn end_sync_sleep(&self) {
        self.sync_sleep_gen.fetch_add(1, Ordering::SeqCst);
    }

    pub fn lock_tracker(&self) -> &LockTracker {
        &self.lock_tracker
    }

    pub fn switch_thread(&self, current: &Thread) {
        if self != current {
            if let Some(ref ctx) = self.memory_context {
                // We have to use active_id here to avoid a mutex.
                ctx.switch_to(self.secctx.active_id());
            } else {
                // Threads with no memory context of their own (the idle thread, kernel
                // threads) must not be left running on the outgoing thread's page tables.
                // That context can be dropped while we sit on it -- ArchContext::drop frees
                // the root page table without checking whether any CPU still has it in cr3 --
                // and once the frame is recycled every translation, including the fault
                // handler's own, resolves through garbage. Switching to the kernel context
                // keeps us on tables that outlive any user context. This is cheap in the
                // common case: switch_to_target skips the cr3 write when it's unchanged, so
                // back-to-back idle switches don't pay for it.
                VirtContext::switch_to_kernel_context();
            }
        }
        // Prologue done, and it is the only part of the switch that takes a tracked lock.
        // `arch_switch_to` takes none, so the attribution window closes here -- on the same cpu
        // that opened it, in straight-line code, rather than depending on where a thread resumes.
        locktrack::leave_switch_window();
        // Give up the outgoing thread's running mark *before* the switch, because `__do_switch`
        // releases its `switch_lock` before acquiring ours -- the moment that store lands, another
        // cpu may take this thread. Clearing it below, where `set_current_thread` otherwise would,
        // is too late: that runs only once this cpu has resumed `self`, so a cpu that legitimately
        // won the lock in between finds the thread still marked running and reports
        // THREAD_CURRENT_ON_TWO_CPUS against an invariant that was never broken. The thread is
        // already queued or parked at this point, so the orphan scan still finds it linked.
        current.set_active_running(false);
        self.arch_switch_to(current);
        // Reached only once `current` has been resumed, which on amd64 means `__do_switch` has won
        // its switch_lock. This cpu now owns it and no other cpu still calls it current, so this is
        // the first point at which publishing is safe -- `switch_to` deliberately does not.
        // (A thread that has never run does not come through here at all; it publishes itself from
        // `new_thread_entry`.)
        unsafe { set_current_thread(current) };
    }

    #[track_caller]
    pub fn do_critical<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Self) -> T,
    {
        self.note_critical_enter(core::panic::Location::caller());
        let res = f(self);
        self.critical_counter.fetch_sub(1, Ordering::SeqCst);
        res
    }

    /// Increment the critical counter, remembering who took it off zero.
    fn note_critical_enter(&self, loc: &'static core::panic::Location<'static>) {
        if self.critical_counter.fetch_add(1, Ordering::SeqCst) == 0 {
            self.critical_origin
                .store(loc as *const _ as usize, Ordering::SeqCst);
        }
    }

    /// The caller that took this thread's critical counter off zero, if it is still nonzero.
    pub fn critical_origin(&self) -> Option<&'static core::panic::Location<'static>> {
        let p = self.critical_origin.load(Ordering::SeqCst);
        (p != 0).then(|| unsafe { &*(p as *const core::panic::Location<'static>) })
    }

    #[inline]
    pub fn is_critical(&self) -> bool {
        self.critical_counter.load(Ordering::SeqCst) > 0
    }

    #[track_caller]
    pub fn exit_critical(&self, loc: &'static core::panic::Location) {
        let res = self.critical_counter.fetch_sub(1, Ordering::SeqCst);
        if res == 0 {
            panic!(
                "critical underflow, critical from {}, exit_critical called from {}",
                loc,
                core::panic::Location::caller()
            );
        }
        assert!(res > 0);
    }

    //#[inline]
    #[track_caller]
    pub fn enter_critical(&self) -> CriticalGuard<'_> {
        let loc = core::panic::Location::caller();
        self.note_critical_enter(loc);
        CriticalGuard { thread: self, loc }
    }

    #[inline]
    #[track_caller]
    pub fn enter_critical_unguarded(&self) {
        self.note_critical_enter(core::panic::Location::caller());
    }

    pub fn maybe_reschedule_thread(&self) {
        /* if we get None here, the thread is either running or blocked, not waiting on a queue. There's a small race condition, here, though,
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
        let Some(ccpu) = self.sched.current_cpu_rq() else {
            return;
        };

        let proc = get_processor(ccpu);
        proc.maybe_wakeup(self);
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

        // Ensure the state write is globally visible before we check conditions and
        // potentially issue a wakeup. Without this fence, on weakly-ordered architectures,
        // the wakeup word write could be observed by another thread before the state write,
        // causing a lost wakeup.
        core::sync::atomic::fence(Ordering::SeqCst);

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
            crate::syscall::sync::requeue_all();
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

    /// True if `addr` lies in a region currently mapped in this thread's memory context.
    ///
    /// Reading an unmapped user address from the kernel faults, and the kernel-mode fault path
    /// has no way to unwind: it resolves to `send_upcall`, which returns to the faulting kernel
    /// instruction, which faults again. Every diagnostic read must therefore be checked first.
    fn diag_addr_mapped(addr: u64) -> bool {
        let Ok(va) = VirtAddr::new(addr) else {
            return false;
        };
        if va.is_kernel() {
            return false;
        }
        let Ok(slot): Result<Slot, _> = va.try_into() else {
            return false;
        };
        current_memory_context().is_some_and(|ctx| ctx.lookup_object(slot).is_some())
    }

    /// DIAG (multiputbug): dump the faulting thread's instruction bytes, stack top, and frame
    /// chain. The kernel shares the user address space here and neither SMAP nor SMEP is
    /// enabled, so these reads are direct — but only ever to addresses that resolve to a mapped
    /// region, since the faulting address itself is by definition not one of those.
    fn dump_fault_context(ip: u64, sp: u64, bp: u64, cx: u64) {
        const SLOT_MASK: u64 = !0x3fff_ffff;
        let page = ip & !0xfff;
        let start = if ip - page >= 8 { ip - 8 } else { page };
        let len = core::cmp::min(24, page + 0x1000 - start) as usize;
        if Self::diag_addr_mapped(start) {
            let mut bytes = [0u8; 24];
            for i in 0..len {
                bytes[i] = unsafe { (start as *const u8).add(i).read_volatile() };
            }
            log::warn!(
                "fault-diag: {} bytes at {:x} (rip {:x}): {:02x?}",
                len,
                start,
                ip,
                &bytes[..len]
            );
        }

        if Self::diag_addr_mapped(sp) {
            let mut stack = [0u64; 8];
            for i in 0..8 {
                stack[i] = unsafe { (sp as *const u64).add(i).read_volatile() };
            }
            log::warn!("fault-diag: stack at rsp {:x}: {:x?}", sp, stack);
        }

        // For the ferroc free-list fault, rcx is the `Shard`. Dumping its header identifies the
        // size class (`obj_size`), which says whether the corrupt link was an interior pointer
        // or an overflow from the preceding block.
        if cx != 0 && cx % 8 == 0 && Self::diag_addr_mapped(cx) {
            let mut words = [0u64; 12];
            for i in 0..12 {
                words[i] = unsafe { (cx as *const u64).add(i).read_volatile() };
            }
            log::warn!("fault-diag: words at rcx {:x}: {:x?}", cx, words);
        }

        let mut fp = bp;
        for depth in 0..8 {
            if fp == 0
                || fp % 8 != 0
                || (fp & SLOT_MASK) != (sp & SLOT_MASK)
                || !Self::diag_addr_mapped(fp)
            {
                break;
            }
            let next = unsafe { (fp as *const u64).read_volatile() };
            let ret = unsafe { (fp as *const u64).add(1).read_volatile() };
            log::warn!("fault-diag: frame {}: fp {:x} ret {:x}", depth, fp, ret);
            if next <= fp {
                break;
            }
            fp = next;
        }
    }

    #[track_caller]
    pub fn send_upcall(self: &ThreadRef, info: UpcallInfo) {
        if !self.is_current_thread() {
            panic!("cannot send upcall to a different thread");
        }
        if self.is_critical() {
            panic!("tried to signal upcall in critical section");
        }

        // A fault taken by the kernel while it is already handling a fault cannot be unwound: the
        // upcall is queued onto the thread's *user* entry frame, so this handler returns to the
        // kernel instruction that faulted and faults again, forever. Bound it: the counter is
        // reset every time the thread actually reaches userspace (`exit_kernel`), so anything
        // past a handful of upcalls without an intervening return is that livelock.
        const MAX_UPCALLS_WITHOUT_RETURN: u32 = 8;
        if self.upcalls_since_user.fetch_add(1, Ordering::SeqCst) >= MAX_UPCALLS_WITHOUT_RETURN {
            log::error!(
                "thread {}: {} upcalls generated without returning to userspace, killing thread. \
                 last: {:?}",
                self.id(),
                MAX_UPCALLS_WITHOUT_RETURN,
                info,
            );
            exit(UPCALL_EXIT_CODE);
        }

        if info.number() != UpcallInfo::Mailbox(0).number() {
            log::warn!(
                "upcall: {}: {:?}, RIP = {:x}, regs = {:?} ctx = {} thread = {}",
                self.id(),
                info,
                self.read_ip(),
                self.read_registers(),
                self.secctx.active_id(),
                self.objid(),
            );
            //crate::panic::backtrace(true, None);
            //loop {}
            if let Ok(regs) = self.read_registers() {
                Self::dump_fault_context(
                    regs.frame.rip,
                    regs.frame.rsp,
                    regs.frame.rbp,
                    regs.frame.rcx,
                );
            }
        }

        let Some(upcall_target) = *self.upcall_target.lock() else {
            exit(UPCALL_EXIT_CODE);
        };

        let num = info.number();

        let Some(options) = upcall_target.options.get(num) else {
            exit(UPCALL_EXIT_CODE);
        };

        if matches!(options.mode, UpcallMode::Abort) {
            if options.flags.contains(UpcallFlags::SUSPEND) {
                self.suspend();
            }
            exit(UPCALL_EXIT_CODE);
        }

        self.arch_queue_upcall(
            upcall_target,
            info,
            matches!(options.mode, UpcallMode::CallSuper),
        );

        // Suspend afterwards to ensure that the upcall frame is queued up.
        if options.flags.contains(UpcallFlags::SUSPEND) {
            self.suspend();
        }
    }

    pub fn must_return_to_user(&self) -> bool {
        self.pending_message.load(Ordering::SeqCst) != 0
            && self.secctx.active_id()
                == self
                    .upcall_target
                    .lock()
                    .map(|u| u.self_ctx)
                    .unwrap_or(0.into())
    }

    pub fn force_exit(self: &ThreadRef) {
        self.flags.fetch_or(THREAD_MUST_EXIT, Ordering::SeqCst);
        if self == current_thread_ref().unwrap() {
            if !self.is_critical() {
                // TODO
                exit(101);
            }
        } else {
            ipi_exec(Destination::AllButSelf, Box::new(|| schedule_resched()));
        }
    }

    pub fn maybe_exit(self: &ThreadRef) {
        if self.flags.load(Ordering::SeqCst) & THREAD_MUST_EXIT != 0 && !self.is_critical() {
            // TODO
            exit(101);
        }
    }

    pub fn set_trace_state(&self, events: u64) -> Result<(), TwzError> {
        if events & PERTHREAD_TRACE_GEN_SAMPLE == 0 {
            if self.sample_expire.lock().take().is_some() {
                log::debug!("clearing tracing sampling for thread {}", self.objid());
            }
        } else {
            log::debug!("setting tracing sampling for thread {}", self.objid());
            *self.sample_expire.lock() =
                Some(crate::clock::get_current_ticks() + SAMPLE_PERIOD_TICKS);
        }
        Ok(())
    }

    pub fn get_trace_state(&self) -> Result<u64, TwzError> {
        let events = if self.sample_expire.lock().is_some() {
            PERTHREAD_TRACE_GEN_SAMPLE
        } else {
            0
        };
        Ok(events)
    }

    pub fn check_sampling(&self) -> bool {
        let mut expire = self.sample_expire.lock();
        let current_ticks = crate::clock::get_current_ticks();
        if expire.is_some() {
            log::trace!(
                "checking sampling for thread {}: {} {}",
                self.objid(),
                expire.unwrap(),
                current_ticks
            );
        }
        if expire.is_some_and(|ex| current_ticks >= ex) {
            *expire = Some(current_ticks + SAMPLE_PERIOD_TICKS);
            if TRACE_MGR.any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_SAMPLE) {
                let data = ThreadSamplingEvent {
                    ip: self.read_ip(),
                    state: self.get_state(),
                };
                let entry = new_trace_entry(
                    TraceKind::Thread,
                    twizzler_abi::trace::THREAD_SAMPLE,
                    TraceEntryFlags::HAS_DATA,
                );
                TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, data));
            }
            true
        } else {
            false
        }
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

#[must_use = "a dropped guard releases immediately; bind it to a variable"]
pub struct CriticalGuard<'a> {
    thread: &'a Thread,
    loc: &'static core::panic::Location<'static>,
}

impl<'a> Drop for CriticalGuard<'a> {
    fn drop(&mut self) {
        self.thread.exit_critical(self.loc);
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        // Only delete the repr if userspace never got its id. `sys_spawn` returns the id with no
        // reference held on it, so deleting here races the spawner's map: the thread can run, exit
        // and be reaped before the spawner ever sees the object, and its map then fails with
        // NoSuchObject. For those threads the spawner owns the object and deletes it explicitly.
        if !self.is_repr_user_owned() {
            self.control_object.object().mark_for_delete();
        }
        if let Some(index) = self.lock_tracker_index {
            deregister_lock_tracker(index);
        }
    }
}

pub fn exit(code: u64) -> ! {
    // TODO: we can do a quick sanity check here that we aren't holding any locks before we exit.
    let th = current_thread_ref().unwrap();
    th.set_state_and_code(ExecutionState::Exited, code);
    remove_thread(th.id());
    log::debug!(
        "thread {} ({}) exits with code {}",
        th.id(),
        th.objid(),
        code
    );
    th.set_is_exiting();
    // A thread can exit with a timeout still outstanding against it -- the callback holds a
    // ThreadRef, so it will run. Retire it here for the same reason the sleep paths do, rather than
    // relying on `schedule_thread_on_cpu`'s is_exiting() check to catch it downstream.
    th.end_sync_sleep();
    th.reset_sync_sleep();
    th.reset_sync_sleep_done();
    th.sync_links.clear_all_references();
    // Disable interrupts for the entire exit sequence including schedule(), to
    // prevent an IPI from rescheduling this thread between cleanup and context switch.
    crate::interrupt::with_disabled(|| {
        crate::syscall::sync::remove_from_requeue(&th);
        schedule(SchedFlags::PREEMPT);
    });
    unreachable!()
}

pub fn get_thread_stats() -> twizzler_abi::syscall::ThreadStats {
    let mut nr_threads = 0;
    let mut nr_running = 0;
    let mut nr_blocked = 0;
    let mut nr_pending_exit = 0;
    crate::processor::sched::with_each_thread(|t| {
        nr_threads += 1;
        match t.get_state() {
            ExecutionState::Running => nr_running += 1,
            ExecutionState::Sleeping => nr_blocked += 1,
            ExecutionState::Exited => nr_pending_exit += 1,
            _ => {}
        }
    });
    twizzler_abi::syscall::ThreadStats {
        nr_threads,
        nr_running,
        nr_blocked,
        nr_pending_exit,
    }
}

pub fn enumerate_objects(buf: &mut [ObjID], offset: usize) -> Result<usize, TwzError> {
    let mut count = 0;
    with_all_threads(|all| {
        all.values()
            .skip(offset)
            .take(buf.len())
            .enumerate()
            .for_each(|(i, t)| {
                buf[i] = t.objid();
                count += 1;
            });
    });
    Ok(count)
}

/// Consecutive scans a thread must be found on no queue before it is called orphaned. The scan runs
/// from the bsp idle loop, so these are separated by real time -- a wake-path handoff is a few
/// instructions and cannot survive one, let alone three.
const ORPHAN_SCANS: u32 = 3;

pub fn check_orphan_threads() {
    //#[cfg(debug_assertions)]
    with_all_threads(|at| {
        for thread in at.values() {
            let is_mutex_linked = thread.mutex_link.is_linked();
            let is_condvar_linked = thread.condvar_link.is_linked();
            let is_requeue_linked = thread.requeue_link.is_linked();
            let is_suspend_linked = thread.suspend_link.is_linked();
            let is_sync_linked = thread.sync_links.is_linked();
            let is_memwait_linked = thread.memwait_link.is_linked();
            let is_sched_linked = thread.sched_link.is_linked();
            let is_timed_wait = thread.has_timed_wait();
            let is_pager_linked = thread.pager_link.is_linked();
            // Running on a cpu is the tenth way to be legitimately off every queue, and it was the
            // one this check could not see -- which made it fire on healthy threads at smp >= 2 and
            // never at smp1. See THREAD_ACTIVE_RUNNING.
            let is_active_running = thread.is_active_running();
            let is_on_any_queue = is_mutex_linked
                || is_condvar_linked
                || is_pager_linked
                || is_requeue_linked
                || is_suspend_linked
                || is_sync_linked
                || is_memwait_linked
                || is_timed_wait
                || is_sched_linked
                || is_active_running;
            // Being off every queue for an instant is normal: every wake path unlinks a thread from
            // where it was sleeping and only then pushes it onto a run queue (`wake_n` ->
            // `add_to_requeue`, `requeue_all` -> `schedule_thread`, `Mutex::release`,
            // `MemoryTracker::wake`, `unsuspend_thread`), and a scan can land in that gap. A lost
            // thread, by contrast, stays lost. So require the condition to persist across scans
            // rather than tagging all seven handoffs. Only this cpu writes the counter.
            if is_on_any_queue || thread.get_state() == ExecutionState::Exited {
                thread.offqueue_scans.store(0, Ordering::Relaxed);
            } else {
                let scans = thread.offqueue_scans.fetch_add(1, Ordering::Relaxed) + 1;
                if scans >= ORPHAN_SCANS {
                    log::warn!(
                        "thread {} ({}) is orphaned: not on any queue and not exited, for {} scans",
                        thread.id(),
                        thread.objid(),
                        scans,
                    );
                }
            }
            log::trace!(
                "[kernel::thread] thread {} ({}) is orphaned: mutex_linked={} condvar_linked={} requeue_linked={} suspend_linked={} sync_linked={} memwait_linked={} timed_wait={} pager_linked={} sched_linked={} active_running={}",
                thread.id(),
                thread.objid(),
                is_mutex_linked,
                is_condvar_linked,
                is_requeue_linked,
                is_suspend_linked,
                is_sync_linked,
                is_memwait_linked,
                is_timed_wait,
                is_pager_linked,
                is_sched_linked,
                is_active_running
            );
        }
    });
}
