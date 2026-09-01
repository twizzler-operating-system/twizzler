use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::UnsafeCell,
    fmt::Debug,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    u32,
};

use intrusive_collections::{RBTreeAtomicLink, linked_list::AtomicLink, offset_of};
use time::{SAMPLE_PERIOD_TICKS, ThreadSched, ThreadStats};
use twizzler_abi::{
    object::{NULLPAGE_SIZE, ObjID, Protections},
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
            virtmem::{Slot, SlotMemo, VirtContext},
        },
    },
    obj::{ThreadSleepLinker, control::ControlObjectCacher},
    processor::{
        ipi::ipi_exec,
        mp::get_processor,
        sched::{SchedFlags, remove_thread, schedule, schedule_resched, with_all_threads},
    },
    security::{
        AccessInfo, KERNEL_SCTX, PermsInfo, SecCtxMgr, SecurityContextRef, SwitchResult,
        kernel_sctx,
    },
    spinlock::Spinlock,
    thread::{
        flags::THREAD_MUST_EXIT,
        kstack::KernelStack,
        locktrack::{LockTracker, deregister_lock_tracker, register_lock_tracker},
        sctx::{SctxCache, Switch},
    },
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

pub mod entry;
mod flags;
pub mod kstack;
pub mod locktrack;
pub mod priority;
pub mod reaper;
mod sctx;
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
    /// Where this thread is blocked in `Mutex::lock`, as a `&'static Location` pointer (0 = not
    /// blocked). Set by `lock` itself rather than by the lock tracker, which is what makes it the
    /// only wait edge available in a build with `DISABLE_LOCK_TRACKING` on -- and that is every
    /// build today, which is why `mutex stall` reports read "edge unknown" without it.
    mutex_wait_at: AtomicUsize,
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
    /// Slot -> object memo for `sys_thread_sync`, in front of the context's `regions` mutex.
    pub slot_memo: SlotMemo,
    pub kernel_stack: KernelStack,
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
    /// Membership in the two global thread registries (`processor::sched`). Separate links because
    /// a thread is in both at once, keyed differently: by [`Thread::id`] and by control-object id.
    pub all_threads_link: RBTreeAtomicLink,
    pub all_threads_repr_link: RBTreeAtomicLink,
    pub sync_links: ThreadSleepLinker,
    pub secctx: SecCtxMgr,
    /// User thread pointer, saved per security context and swapped by [Thread::switch_sctx].
    sctx_cache: SctxCache,
    pub sample_expire: Spinlock<Option<u64>>,
    pub self_reference: UnsafeCell<*mut ThreadRef>,
    pub pending_message: AtomicU64,
    /// Upcalls generated since the thread last returned to userspace. See `send_upcall`.
    pub upcalls_since_user: AtomicU32,
    pub last_pf_addr: AtomicU64,
    pub last_pf_kind: AtomicU32,
    pub last_pf_flags: AtomicU32,
    /// Consecutive faults identical to the one before. Two in a row is ordinary; a fault that
    /// resolves to `Ok` without establishing a mapping produces millions, and nothing else in the
    /// kernel bounds that -- `send_upcall`'s cap only covers the `Err(upcall)` path.
    pub last_pf_count: AtomicU32,
    mutex_count: AtomicU32,
    /// Consecutive orphan scans that found this thread on no queue. Written only by the scanning
    /// cpu. See `check_orphan_threads`.
    offqueue_scans: AtomicU32,
    /// Consecutive scans that found this thread carrying an undelivered force-exit. Same writer
    /// and same purpose as `offqueue_scans`: distinguish a transient from a thread that is
    /// stuck.
    stuck_exit_scans: AtomicU32,
    /// Security context this thread must be running in for a pending force-exit to be delivered,
    /// as (lo, hi); zero means unconditional. Stamped once at spawn from
    /// `ThreadSpawnArgs::home_sctx`, before the thread can run, so it is set before any
    /// THREAD_MUST_EXIT could be.
    ///
    /// A gate call runs the caller's thread inside the callee compartment, holding that
    /// compartment's locks; a force-exit landing there kills the thread with those locks held and
    /// wedges the callee for every compartment. The spawner names the thread's own instance here,
    /// so the exit waits for it to come home.
    exit_sctx: [AtomicU64; 2],
    /// `sync_sleep_gen` as of the last hang scan, and the time (nanos since boot, +1) it last
    /// differed. Written only by the scanning cpu. See `check_system_hang`.
    hang_gen: AtomicU64,
    hang_since: AtomicU64,
    /// Hang reports this thread has already caused, reset whenever it moves. Written only by the
    /// scanning cpu. See `check_system_hang`.
    hang_reports: AtomicU32,
    /// Object and offset of the first sleep word of this thread's current `sys_thread_sync`, as
    /// (lo, hi, offset). Diagnostic only: the user ip of a blocked thread is always the same
    /// syscall wrapper, so it says the thread is in a thread-sync sleep and nothing about *which*
    /// word -- which is exactly what names the userspace lock it is waiting on.
    sleep_word: [AtomicU64; 5],
    /// (site, sync_sleep_gen) of the last winner of `reset_sync_sleep`. Diagnostic only: names
    /// which path consumed the SYNC_SLEEP flag when a hang row shows a parked thread no wake can
    /// claim. Sites: 1=wakeup_word claim, 2=SleepEntry::drop, 3=timeout callback, 4=self
    /// (non-sleep path), 5=device-interrupt claim, 6=self post-sleep cleanup, 7=exit.
    pub(crate) sync_consumer: [AtomicU64; 2],
    /// (site, sync_sleep_gen) of the last requeue-list event involving this thread. Sites:
    /// 1=slow insert, 2=dedup skip (already listed), 3=fast-path direct schedule, 4=requeue_all
    /// claim, 5=claim_own_wakeup removed+won, 6=claim_own_wakeup removed WITHOUT winning (the
    /// eaten-wake candidate), 7=remove_from_requeue. Diagnostic only.
    pub(crate) requeue_event: [AtomicU64; 2],
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
        THREAD_NEWS.fetch_add(1, Ordering::Relaxed);
        /* TODO: guard page support */
        let kernel_stack = KernelStack::new();
        let id = ID_COUNTER.next();
        let lock_tracker = Arc::new(LockTracker::new(id.value()));
        let lock_tracker_index = register_lock_tracker(lock_tracker.clone());
        Self {
            arch: crate::arch::thread::ArchThread::new(),
            priority: AtomicU32::new(priority.raw()),
            stable_priority: AtomicU32::new(priority.raw()),
            id,
            flags: AtomicU32::new(0),
            kernel_stack,
            critical_counter: AtomicU64::new(0),
            critical_origin: AtomicUsize::new(0),
            mutex_wait_at: AtomicUsize::new(0),
            switch_lock: AtomicU64::new(0),
            has_run: AtomicBool::new(false),
            sync_sleep_gen: AtomicU64::new(0),
            donated_priority: AtomicU32::new(u32::MAX),
            stats: ThreadStats::new(crate::processor::sched::current_stat_ticks()),
            memory_context: ctx,
            slot_memo: SlotMemo::new(),
            spawn_args,
            control_object: ControlObjectCacher::new(ThreadRepr::default()),
            sched_link: AtomicLink::default(),
            mutex_link: AtomicLink::default(),
            memwait_link: AtomicLink::default(),
            suspend_link: RBTreeAtomicLink::default(),
            requeue_link: RBTreeAtomicLink::default(),
            condvar_link: RBTreeAtomicLink::default(),
            all_threads_link: RBTreeAtomicLink::default(),
            all_threads_repr_link: RBTreeAtomicLink::default(),
            pager_link: AtomicLink::default(),
            sync_links: ThreadSleepLinker::new(),
            upcall_target: Spinlock::new(None),
            secctx: SecCtxMgr::new_kernel(),
            // Threads start in the kernel context; `start_new_user` re-seeds this from the
            // spawning thread when it clones the attachment set.
            sctx_cache: SctxCache::new(KERNEL_SCTX, &kernel_sctx()),
            sample_expire: Spinlock::new(None),
            self_reference: UnsafeCell::new(core::ptr::null_mut()),
            sched: ThreadSched::default(),
            pending_message: AtomicU64::new(0),
            upcalls_since_user: AtomicU32::new(0),
            last_pf_addr: AtomicU64::new(0),
            last_pf_count: AtomicU32::new(0),
            last_pf_kind: AtomicU32::new(0),
            offqueue_scans: AtomicU32::new(0),
            stuck_exit_scans: AtomicU32::new(0),
            exit_sctx: [const { AtomicU64::new(0) }; 2],
            hang_gen: AtomicU64::new(0),
            hang_since: AtomicU64::new(0),
            hang_reports: AtomicU32::new(0),
            sleep_word: [const { AtomicU64::new(0) }; 5],
            sync_consumer: [const { AtomicU64::new(0) }; 2],
            requeue_event: [const { AtomicU64::new(0) }; 2],
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

    /// Switch the active security context, carrying the user thread pointer with it.
    ///
    /// A compartment's TLS is only reachable from inside that compartment, so the thread pointer
    /// belongs to the (thread, context) pair rather than to the thread: this saves the outgoing
    /// context's pointer and installs the incoming one, which is zero the first time this thread
    /// runs in a given context.
    ///
    /// Returns the installed pointer, which is what lets a cross-compartment entry decide from the
    /// switch alone whether it may use TLS immediately or must build a region first -- userspace
    /// cannot test for a zero thread pointer, because the read that would test it is the fault.
    pub fn switch_sctx(&self, id: ObjID) -> (SwitchResult, u64) {
        // One lock acquisition covers the whole fast path: reading the outgoing context, saving
        // its thread pointer, finding the incoming one, and installing it as active.
        match self.sctx_cache.switch(id, self.get_tls()) {
            Switch::NoSwitch => {
                // Still load the page tables: being active in a context is not by itself proof
                // that its root is in cr3.
                self.memory_context.as_ref().map(|mc| mc.switch_to(id));
                (SwitchResult::NoSwitch, self.get_tls())
            }
            Switch::Hit(hit) => {
                self.set_tls(hit.tls);
                if let Some(mc) = self.memory_context.as_ref() {
                    // Safety: the target came from this context, and the `Hit` holds a reference
                    // to the security context that keeps its registration alive across this call.
                    unsafe { mc.switch_to_target(&hit.target) };
                }
                (SwitchResult::Switched, hit.tls)
            }
            Switch::Miss { from } => self.switch_sctx_slow(from, id),
        }
    }

    /// The incoming context was not cached, so go through the attached map.
    ///
    /// The cache has already saved `from`'s thread pointer and left it active, so a `NotAttached`
    /// bail-out here leaves everything as it was.
    #[inline(never)]
    fn switch_sctx_slow(&self, from: ObjID, id: ObjID) -> (SwitchResult, u64) {
        let Some(ctx) = self.secctx.attached(id) else {
            return (SwitchResult::NotAttached, self.get_tls());
        };
        self.sctx_cache.save_tls(from, self.get_tls());
        let new_tls = self.sctx_cache.saved_tls(id);
        self.sctx_cache.set_active(id, &ctx);
        self.set_tls(new_tls);
        if let Some(mc) = self.memory_context.as_ref() {
            mc.switch_to(id);
            // Cache what that cost, so the next switch into this context -- and the return trip,
            // once it has been through here once itself -- takes the fast path.
            if let Some(target) = mc.switch_target(id) {
                self.sctx_cache.insert(id, new_tls, &ctx, target);
            }
        }
        (SwitchResult::Switched, new_tls)
    }

    /// The id of the security context this thread is running in.
    pub fn active_sctx_id(&self) -> ObjID {
        self.sctx_cache.active_id()
    }

    /// The security context this thread is running in.
    pub fn active_sctx(&self) -> SecurityContextRef {
        // The attached map holds a strong reference to whatever is active, so the weak one in the
        // cache resolves; going to the map is a belt-and-braces fallback, not an expected path.
        self.sctx_cache
            .active_ctx()
            .or_else(|| self.secctx.attached(self.active_sctx_id()))
            .unwrap_or_else(kernel_sctx)
    }

    /// Check access rights in this thread's active context.
    pub fn check_active_access(
        &self,
        access_info: &AccessInfo,
        default_prots: Protections,
    ) -> PermsInfo {
        self.active_sctx()
            .lookup(access_info.target_id, default_prots)
    }

    /// Search every context this thread is attached to for one granting the requested access.
    pub fn search_access(&self, access_info: &AccessInfo, default_prots: Protections) -> PermsInfo {
        self.secctx
            .search_access(&self.active_sctx(), access_info, default_prots)
    }

    /// Seed the active context, for a thread built by cloning another's attachments.
    pub fn init_active_sctx(&self, ctx: &SecurityContextRef) {
        self.sctx_cache.set_active(ctx.id(), ctx);
    }

    /// Mark this thread as having executed, returning whether it already had. A thread that has
    /// not run yet exists on exactly one run queue and has never been current anywhere.
    pub fn mark_run(&self) -> bool {
        self.has_run.swap(true, Ordering::SeqCst)
    }

    /// True once this thread's switch has saved its stack pointer and is provably no longer
    /// executing on its kernel stack.
    ///
    /// `is_active_running` does *not* answer this, and reading it as if it did is what let
    /// `drain_exited` free a stack out from under its owner: `switch_thread` clears that flag
    /// *before* `arch_switch_to`, on purpose (see `set_active_running`), so it reads false while
    /// the thread is still running `save_extended_state` and pushing registers inside
    /// `__do_switch`. `switch_lock` is the real release point -- `__do_switch` stores 0 to it
    /// immediately after `mov [rsi], rsp`, behind an `sfence`, so observing 0 means the saved rsp
    /// and every push before it are visible and the stack is dead.
    pub fn has_left_kernel_stack(&self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            self.switch_lock.load(Ordering::SeqCst) == 0
        }
        // aarch64's `arch_switch_to` never touches `switch_lock`, so it reads 0 the whole time a
        // thread is running and would answer "yes" always. Saying "no" instead costs cross-cpu
        // reaping there -- `Processor::cleanup_exited` on the owning cpu still runs, which is the
        // behaviour that predates the reaper thread, leak and all. Restoring it means giving that
        // switch a release point of its own, not relaxing this.
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
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
                ctx.switch_to(self.active_sctx_id());
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
        self.critical_counter.fetch_sub(1, Ordering::AcqRel);
        res
    }

    /// Increment the critical counter, remembering who took it off zero.
    ///
    /// The counter is `AcqRel`: it is the guard, and the section it opens must not be hoisted
    /// above it. The origin beside it is `Relaxed` -- diagnostic, only ever read to name a site in
    /// a report, and a `SeqCst` store of a `&'static Location` is an `xchg` charged to every
    /// critical section (several per wake).
    fn note_critical_enter(&self, loc: &'static core::panic::Location<'static>) {
        if self.critical_counter.fetch_add(1, Ordering::AcqRel) == 0 {
            self.critical_origin
                .store(loc as *const _ as usize, Ordering::Relaxed);
        }
    }

    /// The caller that took this thread's critical counter off zero, if it is still nonzero.
    pub fn critical_origin(&self) -> Option<&'static core::panic::Location<'static>> {
        let p = self.critical_origin.load(Ordering::Relaxed);
        (p != 0).then(|| unsafe { &*(p as *const core::panic::Location<'static>) })
    }

    #[inline]
    pub fn is_critical(&self) -> bool {
        self.critical_counter.load(Ordering::Acquire) > 0
    }

    #[track_caller]
    pub fn exit_critical(&self, loc: &'static core::panic::Location) {
        let res = self.critical_counter.fetch_sub(1, Ordering::AcqRel);
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
        //
        // Split from the wake itself so the *skips* are visible. The criticality test below is on
        // the calling thread, not on `self`, and dropping the wake is silent and final -- nothing
        // retries it, so anyone sleeping on this repr sleeps through a change that already
        // happened. See `diag::STATE_WAKE_SKIPPED_CRITICAL`.
        let wake_worthy = !(old_state == ExecutionState::Running
            && state == ExecutionState::Sleeping
            || old_state == ExecutionState::Sleeping && state == ExecutionState::Running)
            && (old_state != state
                || state == ExecutionState::Exited
                || state == ExecutionState::Suspended)
            && old_state != ExecutionState::Exited;
        if !wake_worthy {
            return;
        }
        let cur = current_thread_ref();
        // Exactly the original `!current_thread_ref().map_or(true, |ct| ct.is_critical())`.
        if !cur.as_ref().is_some_and(|ct| !ct.is_critical()) {
            let counter = match cur {
                Some(_) => Some(&locktrack::diag::STATE_WAKE_SKIPPED_CRITICAL),
                // Only a probe once threading is up: before that there is legitimately no current
                // thread, and nothing is waiting on this repr yet either.
                None => locktrack::diag::threading_up()
                    .then_some(&locktrack::diag::STATE_WAKE_SKIPPED_NO_THREAD),
            };
            if let Some(counter) = counter
                && counter.hit()
            {
                // emerglogln takes no console lock, which is what makes it usable from here: in the
                // case worth reporting the caller is critical by construction. The counter's own
                // name carries which of the two skips this was; `this_thread()` is u64::MAX when
                // there is no caller at all.
                emerglogln!(
                    "thread {} ({}) {:?} -> {:?}: {} (caller thread {})",
                    self.id(),
                    self.objid(),
                    old_state,
                    state,
                    counter.name(),
                    locktrack::diag::this_thread(),
                );
            }
            return;
        }
        self.control_object
            .object()
            .wakeup_word(NULLPAGE_SIZE + offset_of!(ThreadRepr, status), usize::MAX);
        crate::syscall::sync::requeue_all();
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
        if self.upcalls_since_user.fetch_add(1, Ordering::Relaxed) >= MAX_UPCALLS_WITHOUT_RETURN {
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
                self.active_sctx_id(),
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
            && self.active_sctx_id()
                == self
                    .upcall_target
                    .lock()
                    .map(|u| u.self_ctx)
                    .unwrap_or(0.into())
    }

    pub fn force_exit(self: &ThreadRef) {
        self.flags.fetch_or(THREAD_MUST_EXIT, Ordering::SeqCst);
        if self == current_thread_ref().unwrap() {
            // `exit_sctx_ok` for the same reason the target path defers: a thread asking to die
            // while executing inside another compartment would leave that compartment's locks
            // held. The flag is sticky, so a later poll takes it.
            if !self.is_critical() && self.exit_sctx_ok() {
                // TODO
                exit(101);
            }
        } else {
            // The resched below only reaches a target that is running. One already parked in a
            // thread-sync sleep polls neither exit point and nothing else will wake it, so hand it
            // to the timeout queue.
            //
            // Deferred rather than claimed here: `prep_sleep` sets SYNC_SLEEP while the thread is
            // still running, before its critical guard, so a `reset_sync_sleep` from this side can
            // requeue a thread that is already on the run queue -- "attempted to insert an object
            // that is already linked". On the timeout thread the generation captured now is
            // re-checked first (see `end_sync_sleep`), so a sleep that ended meanwhile is left
            // alone, and a target still mid-commit is critical and merely lands on the requeue list
            // for its own `claim_own_wakeup` to collect.
            let _ = crate::clock::register_timeout_callback(
                crate::syscall::sync::force_exit_wake_ns(self),
                crate::syscall::sync::thread_sync_cb_timeout,
                self.clone(),
                self.sync_sleep_gen(),
            );
            ipi_exec(
                Destination::AllButSelf,
                Box::new(|| schedule_resched()),
                true,
            );
        }
    }

    /// Record the word this thread is about to sleep on, for the wait table. Diagnostic only.
    ///
    /// The armed compare value and op modifiers ride along so the table can re-evaluate the sleep
    /// predicate against the word's *current* value: a parked thread whose predicate no longer
    /// holds is a lost wake, caught in the transcript rather than argued about afterwards.
    pub fn note_sleep_word(&self, obj: ObjID, offset: usize, value: u64, is32: bool, invert: bool) {
        let parts = obj.parts();
        self.sleep_word[0].store(parts[0], Ordering::Relaxed);
        self.sleep_word[1].store(parts[1], Ordering::Relaxed);
        self.sleep_word[2].store(offset as u64, Ordering::Relaxed);
        self.sleep_word[3].store(value, Ordering::Relaxed);
        self.sleep_word[4]
            .store((is32 as u64) | ((invert as u64) << 1), Ordering::Relaxed);
    }

    /// Record which path just won `reset_sync_sleep` on this thread; see the field. Called by the
    /// winner immediately after the win, so the (site, gen) pair identifies the consumer of the
    /// park a hang row later shows as unclaimable.
    pub fn note_sync_consumer(&self, site: u64) {
        self.sync_consumer[0].store(site, Ordering::Relaxed);
        self.sync_consumer[1].store(self.sync_sleep_gen(), Ordering::Relaxed);
    }

    /// Record the last requeue-list event for this thread; see the field.
    pub fn note_requeue_event(&self, site: u64) {
        self.requeue_event[0].store(site, Ordering::Relaxed);
        self.requeue_event[1].store(self.sync_sleep_gen(), Ordering::Relaxed);
    }

    /// Record the security context this thread must be running in before a pending force-exit is
    /// delivered. Zero clears the restriction.
    pub fn set_exit_sctx(&self, sctx: ObjID) {
        let parts = sctx.parts();
        self.exit_sctx[0].store(parts[0], Ordering::SeqCst);
        self.exit_sctx[1].store(parts[1], Ordering::SeqCst);
    }

    /// Whether a pending force-exit may be delivered here, as far as the security context goes.
    ///
    /// False means the thread is executing inside some other compartment -- a gate call -- and
    /// exiting would leave that compartment's locks held forever. It returns home, and the flag is
    /// sticky, so this only delays the exit.
    pub fn exit_sctx_ok(&self) -> bool {
        let want = ObjID::from_parts([
            self.exit_sctx[0].load(Ordering::SeqCst),
            self.exit_sctx[1].load(Ordering::SeqCst),
        ]);
        if want.raw() == 0 {
            return true;
        }
        self.active_sctx_id() == want
    }

    /// A force-exit is pending *and* can be delivered at this thread's current security context.
    /// Callers that decline to block on behalf of an exiting thread want this, not `must_exit`:
    /// an exit that cannot be delivered yet is not a reason to spin.
    pub fn exit_deliverable(&self) -> bool {
        self.must_exit() && self.exit_sctx_ok()
    }

    pub fn maybe_exit(self: &ThreadRef) {
        if !self.must_exit() || self.is_critical() {
            return;
        }
        if !self.exit_sctx_ok() {
            locktrack::diag::EXIT_DEFERRED_SCTX.count_only();
            return;
        }
        // Exiting from here would leak every kernel mutex this thread holds: `exit` unlinks it from
        // the scheduler but releases nothing it owns, and every later locker then sleeps behind an
        // owner that will never run. That is the `VirtContext::secctx` pile-up -- a thread
        // force-exited inside `with_arch` left 261 waiters on a lock nobody could release.
        //
        // Deferring is safe because MUST_EXIT is sticky: the next poll after the last unlock takes
        // it. `is_critical` above is the same argument for spinlocks; this is the mutex half, which
        // that check does not cover.
        if self.get_mutex_count() > 0 {
            locktrack::diag::EXIT_DEFERRED_MUTEX_HELD.count_only();
            return;
        }
        // Same argument, for sleep links. Only the `sys_thread_sync` frame knows which words this
        // thread is parked on -- `undo_sleep` walks the caller's op array -- so exiting from here
        // abandons the one thing that could unlink them, and `clear_all_references` then frees the
        // slab under nodes still in objects' sleep trees.
        //
        // `must_not_block` already states the intended discipline: a thread that must exit returns
        // without blocking, runs its normal cleanup, and exits from `sys_thread_sync` afterwards.
        // This is the arm that was bypassing it.
        if self.sync_links.is_linked() {
            locktrack::diag::EXIT_DEFERRED_SLEEP_LINKED.count_only();
            return;
        }
        // TODO
        exit(101);
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

/// Alloc/drop totals for `Thread` structs, printed by the pressure census. A `Thread` alive
/// past reaping is invisible to every queue/registry counter, but it pins its `SecCtxMgr`'s
/// attached security contexts (and their mappings) -- a growing news-drops gap under churn is
/// the leak the census is hunting (pagerwedge.md §3.8).
pub static THREAD_NEWS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
pub static THREAD_DROPS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

impl Drop for Thread {
    fn drop(&mut self) {
        THREAD_DROPS.fetch_add(1, Ordering::Relaxed);
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

/// Sizes of the per-thread kernel heap allocations, so a `kalloc_census` size class can be read
/// back to a struct without guessing. Printed once at boot under `--diag`.
pub fn log_thread_sizes() {
    logln!(
        "[thread] sizeof Thread={} LockTracker={} LockTrackerInner={} ArchThread={}",
        core::mem::size_of::<Thread>(),
        core::mem::size_of::<crate::thread::locktrack::LockTracker>(),
        crate::thread::locktrack::inner_size(),
        core::mem::size_of::<crate::arch::thread::ArchThread>(),
    );
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
    // Nothing below unlinks `mutex_link`, and there is no back-pointer from a thread to the mutex
    // whose sleep queue it is in, so a thread that dies here stays a member of that queue forever.
    // `release` will later hand it the mutex (`pop_highest_priority` does not skip the dead), and
    // every later locker sleeps behind an owner that will never take it -- `lock`'s
    // MUTEX_HANDOFF_TO_DEAD arm reclaims that, but only once someone else contends.
    //
    // Reported from `exit` rather than from the ChangeState syscall so it covers every exit path.
    // A hit says the "dies while queued" reading of the shutdown hang is live; the queue's own
    // `pri` is now stale too, since it was computed with this thread in the list.
    if th.mutex_link.is_linked() && locktrack::diag::EXIT_WHILE_MUTEX_QUEUED.hit() {
        emerglogln!(
            "thread {} ({}) exiting while still queued on a mutex (mutex_wait {}, mutex_count {}, code {})",
            th.id(),
            th.objid(),
            th.get_mutex_wait(),
            th.get_mutex_count(),
            code,
        );
        th.print_locks();
        crate::panic::backtrace(true, None);
    }
    // A thread can exit with a timeout still outstanding against it -- the callback holds a
    // ThreadRef, so it will run. Retire it here for the same reason the sleep paths do, rather than
    // relying on `schedule_thread_on_cpu`'s is_exiting() check to catch it downstream.
    th.end_sync_sleep();
    if th.reset_sync_sleep() {
        th.note_sync_consumer(7);
    }
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
        nr_exited_backlog: crate::processor::EXITED_BACKLOG.load(Ordering::Relaxed),
        nr_reaped: crate::processor::REAPED.load(Ordering::Relaxed),
    }
}

pub fn enumerate_objects(buf: &mut [ObjID], offset: usize) -> Result<usize, TwzError> {
    let mut count = 0;
    with_all_threads(|all| {
        all.iter()
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

/// Scans a thread may carry an undelivered force-exit before it is reported. Longer than
/// [`ORPHAN_SCANS`] because a thread legitimately takes a moment to notice: it has to reach a poll
/// point, and one blocked on a real reply is entitled to wait for it. What is not legitimate is
/// still carrying the flag many scans later, which is the shutdown hang.
const STUCK_EXIT_SCANS: u32 = 20;

/// How long one thread must sit in a single uninterrupted thread-sync sleep before the wait table
/// below is printed. Long waits are ordinary here -- sshd waiting for a connection, the cleaner's
/// 8s poll -- so this is set past anything the test suite does on purpose.
/// Upper bound on rows in the hang table, and therefore on the `ThreadRef`s held across the
/// print. A bench boot can have thousands of threads; the table exists to identify a wedge, not to
/// enumerate a workload. Overflow is reported rather than dropped.
const HANG_TABLE_MAX_THREADS: usize = 64;

/// Set under the *lifetime of the thing being diagnosed*, not just under "longer than a legitimate
/// wait". At 25s this could never fire for a sysbench net peer, which gives up and exits at 20s --
/// so the one detector that asks exactly the right question (has this thread's `sync_sleep_gen`
/// stopped moving while it is `Sleeping`?) was structurally blind to the case it was needed for.
/// 6s leaves room for the two scans a report requires inside that window.
///
/// Service threads parked on a condvar now cross this in every boot. That is affordable because
/// what each one costs is a single line (see `StuckRecord`), not a table: the expensive table is
/// still rationed by [`MAX_HANG_REPORTS`].
const HANG_REPORT_SECS: u64 = 6;

/// What a newly-stuck thread reports, gathered under `with_all_threads`' spinlock and printed
/// after it drops.
///
/// Plain `Copy` scalars rather than a `ThreadRef` on purpose: cloning references here would put
/// their drops -- `IdCounter::release`, a *sleeping* mutex -- on a path that has just walked the
/// thread list, and printing under the lock would take the console lock beneath a spinlock.
///
/// These fields are chosen to answer one question: parked, or runnable and not run? `Sleeping` with
/// `sync`/`timed` set is parked on a wait nobody has satisfied; `sched` set is on a run queue and
/// not getting cpu.
#[derive(Clone, Copy)]
struct StuckRecord {
    id: u64,
    objid: ObjID,
    /// The compartment. Without it a record cannot be attributed to the failing peer, which is the
    /// only reason to be reading these at all -- the rationed table carries the name, but its rows
    /// come back nameless far more often than not.
    sctx: ObjID,
    state: ExecutionState,
    /// Executing on a cpu at scan time. With `sched`, this is the discriminator the whole change
    /// is for: `sched` set and `active` clear over the window is ready-but-never-run.
    active: bool,
    ip: u64,
    sync: bool,
    timed: bool,
    sched: bool,
    requeue: bool,
    condvar: bool,
    mutex: bool,
    pager: bool,
    memwait: bool,
}

/// Newly-stuck threads recorded per scan. One line each, and a thread only earns one per episode
/// (see [`MAX_THREAD_HANG_REPORTS`]), so this bounds a scan rather than the boot.
const HANG_STUCK_MAX: usize = 32;
/// Tables one thread may cause per stuck episode, where an episode ends when the thread moves.
///
/// A thread parked legitimately never moves, so it spends this much and then goes quiet for the
/// rest of the boot. That is the whole point: without it, `MAX_HANG_REPORTS` is a shared budget and
/// the threads that cross the threshold *first* are exactly the ones entitled to -- sshd, the
/// cleaner, the service threads that park early and stay parked. A sweep transcript shows the
/// result: four tables at 25s, all for healthy threads, and then silence through a real wedge five
/// minutes later that had every cpu halted and nothing left to report it.
const MAX_THREAD_HANG_REPORTS: u32 = 1;
/// Cap on the total, so the per-thread rule above cannot be multiplied into a stream by a system
/// with many parked threads. Set well above the number that park in a healthy boot, since spending
/// it on them is the failure this is built around.
const MAX_HANG_REPORTS: u32 = 16;

static HANG_REPORTS: AtomicU32 = AtomicU32::new(0);
/// Passes of [check_system_hang], for the heartbeat that makes its silence readable.
static HANG_SCANS: AtomicU64 = AtomicU64::new(0);

/// Print where every thread is parked, once any one of them has been in the same thread-sync sleep
/// for [`HANG_REPORT_SECS`].
///
/// The wedges this exists for leave every cpu halted and every thread `Sleeping`, which the state
/// list alone cannot take apart: it says they are all blocked and nothing about *on what*. The
/// linkage bits name the queue (sync = a thread-sync sleep, pager = an inflight pager reply,
/// memwait = the tracker, mutex/condvar = a kernel lock), and the user ip plus the active security
/// context name the userspace call -- which for a gate call is the caller's ip in the callee's
/// context, i.e. exactly the thing a transcript of a stuck `CompartmentHandle::lookup` has never
/// contained.
///
/// Per-thread rather than system-wide, because neither system-wide signal survives contact with a
/// real wedge: "no thread is Running" fails because the transcripts do carry a thread in state
/// Running, and "no syscalls anywhere" fails because a wedged system still has the heartbeat thread
/// and the monitor's watchdog waking on their timers. Only the individual thread stops moving.
pub fn check_system_hang() {
    // +1 so a zero timestamp always means "no sample yet".
    let now = crate::instant::Instant::now().into_time_span().as_nanos() as u64 + 1;
    let mut any_stuck = false;
    let mut stuck_id = None;
    let mut stuck: heapless::Vec<StuckRecord, HANG_STUCK_MAX> = heapless::Vec::new();
    let mut stuck_unrecorded = 0usize;
    let mut examined = 0usize;
    with_all_threads(|at| {
        for thread in at.iter() {
            if thread.is_idle_thread() {
                continue;
            }
            examined += 1;
            let sleep_gen = thread.sync_sleep_gen();
            // Not `state != Sleeping`, which is what this asked before. `Running` in this kernel
            // covers *runnable and sitting on a run queue* as well as on-cpu, so requiring
            // `Sleeping` made the scan structurally blind to a thread that is ready and simply
            // never scheduled -- one of the two hypotheses it exists to tell apart. It answered
            // "none of those" by construction, which reads as evidence and is not.
            //
            // On-cpu is progress by definition, and an exited thread is done; everything else stays
            // in the window and is classified by the flags on its line rather than filtered out
            // here. The cost is real: a pure-compute thread never advances `sync_sleep_gen` and is
            // off-cpu whenever a scan misses its quantum, so it can trip this. Read the new arm by
            // its `active`/`sched` flags, never by counting it.
            let progressing =
                thread.is_active_running() || thread.get_state() == ExecutionState::Exited;
            if thread.hang_gen.swap(sleep_gen, Ordering::Relaxed) != sleep_gen || progressing {
                thread.hang_since.store(now, Ordering::Relaxed);
                // Having moved is what earns a thread its voice back. A thread that ran and then
                // stopped is the one worth a table; a thread that has been parked since boot has
                // already had its say, and this is what stops it from taking the budget again
                // every 25s for the rest of the run.
                thread.hang_reports.store(0, Ordering::Relaxed);
                continue;
            }
            let since = thread.hang_since.load(Ordering::Relaxed);
            if since == 0 {
                thread.hang_since.store(now, Ordering::Relaxed);
                continue;
            }
            if now.saturating_sub(since) >= HANG_REPORT_SECS * 1_000_000_000 {
                // Restart this thread's window first, so the early return below still puts a
                // permanently parked thread on an interval rather than on every scan.
                thread.hang_since.store(now, Ordering::Relaxed);
                // Only the scanning cpu writes this, so load/store needs no atomicity beyond the
                // field's; it also keeps a thread parked for the life of the boot from counting
                // past the cap forever.
                let reports = thread.hang_reports.load(Ordering::Relaxed);
                if reports >= MAX_THREAD_HANG_REPORTS {
                    continue;
                }
                thread.hang_reports.store(reports + 1, Ordering::Relaxed);
                // Record *who* tripped it, not just that something did. Without this the header
                // says a thread is stuck and the table lists every thread sorted by id, so the one
                // that actually crossed the threshold is indistinguishable from the dozens parked
                // legitimately -- which is how the first attempt at filtering this got aimed at the
                // wrong threads entirely.
                stuck_id = Some((thread.id(), thread.objid()));
                any_stuck = true;
                let rec = StuckRecord {
                    id: thread.id(),
                    objid: thread.objid(),
                    sctx: thread.active_sctx_id(),
                    state: thread.get_state(),
                    active: thread.is_active_running(),
                    ip: thread.read_ip(),
                    sync: thread.sync_links.is_linked(),
                    timed: thread.has_timed_wait(),
                    sched: thread.sched_link.is_linked(),
                    requeue: thread.requeue_link.is_linked(),
                    condvar: thread.condvar_link.is_linked(),
                    mutex: thread.mutex_link.is_linked(),
                    pager: thread.pager_link.is_linked(),
                    memwait: thread.memwait_link.is_linked(),
                };
                if stuck.push(rec).is_err() {
                    stuck_unrecorded += 1;
                }
            }
        }
    });
    // Ahead of the budget check, and never rationed by it. The table below is capped at
    // [`MAX_HANG_REPORTS`] for the whole boot, and the threads that cross the threshold *first* are
    // the ones that park early and legitimately -- so on a long boot the budget is spent before
    // anything interesting happens, and the detector goes quiet in a way indistinguishable from
    // "nothing was stuck". These lines cannot be exhausted that way: one per thread per episode.
    for rec in &stuck {
        emerglogln!(
            "[hang] thread {} ({}) sctx {} unmoved for {}s: {:?} active {} ip {:x} | sync {} timed {} sched {} requeue {} condvar {} mutex {} pager {} memwait {}",
            rec.id,
            rec.objid,
            rec.sctx,
            HANG_REPORT_SECS,
            rec.state,
            rec.active,
            rec.ip,
            rec.sync,
            rec.timed,
            rec.sched,
            rec.requeue,
            rec.condvar,
            rec.mutex,
            rec.pager,
            rec.memwait,
        );
    }
    if stuck_unrecorded > 0 {
        emerglogln!(
            "[hang] ... and {} further threads this scan could not record (cap {})",
            stuck_unrecorded,
            HANG_STUCK_MAX
        );
    }
    // A scan that finds nothing and a scan that never ran are both silent, and the difference is
    // the whole result when the question is "was this compartment sampled?". This runs from the
    // bsp idle loop, so during a benchmark it may genuinely not run for long stretches -- say so
    // periodically rather than leaving the reader to guess which kind of silence they have.
    let scans = HANG_SCANS.fetch_add(1, Ordering::Relaxed) + 1;
    if scans == 1 || scans % 16 == 0 {
        emerglogln!(
            "[hang] scan #{}: {} threads examined, {} newly stuck",
            scans,
            examined,
            stuck.len()
        );
    }
    if !any_stuck {
        return;
    }
    // Say so once, rather than returning silently: with the budget gone, no table and no wedge look
    // identical in a transcript.
    let spent = HANG_REPORTS.fetch_add(1, Ordering::Relaxed);
    if spent >= MAX_HANG_REPORTS {
        if spent == MAX_HANG_REPORTS {
            emerglogln!(
                "[hang] wait-table budget exhausted ({} printed); further reports are the lines above only",
                MAX_HANG_REPORTS
            );
        }
        return;
    }
    let (stuck_tid, stuck_objid) = stuck_id.unwrap_or((0, 0.into()));
    emerglogln!(
        "== thread {} ({}) has been asleep for {}s; thread wait table:",
        stuck_tid,
        stuck_objid,
        HANG_REPORT_SECS
    );
    // Snapshot the thread list, then print outside the lock. The name lookup below ends in
    // `ControlObjectCacher::summarize`, which takes a **mutex**, while `with_all_threads` holds a
    // spinlock -- so doing it inline panics with "cannot lock mutex in critical context" the first
    // time a repr object is actually findable. That is not hypothetical: it killed 10/10 sysbench
    // boots on 2026-08-27, always on the first thread reaching the lookup, so the table printed
    // its header and no rows.
    let mut threads: heapless::Vec<ThreadRef, HANG_TABLE_MAX_THREADS> = heapless::Vec::new();
    let mut untabled = 0usize;
    with_all_threads(|at| {
        // `clone_pointer` rather than `iter()`: the iterator yields borrows that cannot outlive
        // the guard, and the whole point here is to outlive it.
        let mut cursor = at.front();
        while let Some(thread) = cursor.clone_pointer() {
            cursor.move_next();
            if thread.is_idle_thread() {
                continue;
            }
            if threads.push(thread).is_err() {
                untabled += 1;
            }
        }
    });
    {
        for thread in threads.iter() {
            // The runtime mirrors thread names into notes on the repr object; without one the
            // table is a list of anonymous parked threads (rustchang.md).
            let mut namebuf = [0u8; 24];
            let namelen =
                match crate::obj::lookup_object(thread.objid(), crate::obj::LookupFlags::empty()) {
                    crate::obj::LookupResult::Found(obj) => obj.get_notes().summarize(&mut namebuf),
                    _ => 0,
                };
            // Re-evaluate the recorded sleep predicate against the word's current value. `lost`
            // true on a row that is Sleeping with sync linked means the thread should be awake:
            // the word moved past its armed value and no wake ever claimed it. `slprs` is the
            // word object's sleeper count -- zero while this row is parked in its tree indicts
            // `wakeup_word`'s sleepers==0 fast path specifically. Advisory (all loads racy);
            // the raw values are printed so a transcript can be re-judged.
            let sw_obj = ObjID::from_parts([
                thread.sleep_word[0].load(Ordering::Relaxed),
                thread.sleep_word[1].load(Ordering::Relaxed),
            ]);
            let sw_off = thread.sleep_word[2].load(Ordering::Relaxed) as usize;
            let sw_val = thread.sleep_word[3].load(Ordering::Relaxed);
            let sw_meta = thread.sleep_word[4].load(Ordering::Relaxed);
            let (cur, slprs, cw, wt) = if sw_obj != 0.into() {
                match crate::obj::lookup_object(sw_obj, crate::obj::LookupFlags::empty()) {
                    crate::obj::LookupResult::Found(obj) => {
                        let cur = if sw_meta & 1 != 0 {
                            obj.read_atomic_32(sw_off).ok().map(|v| v as u64)
                        } else {
                            obj.read_atomic_64(sw_off).ok()
                        };
                        // Context words for a queue-bell sleeper: in RawQueueHdr the bell sits at
                        // +0x100 with consumer_waiting at +0xC0 and waiters at +0x80, so bell-0x40
                        // and bell-0x80 are the producer-visible arm state. For any other word
                        // these are just nearby memory -- read them, label them, let the reader
                        // decide; they only mean something when the offset is a known bell.
                        let (cw, wt) = if sw_off >= 0x80 {
                            (
                                obj.read_atomic_32(sw_off - 0x40).ok(),
                                obj.read_atomic_32(sw_off - 0x80).ok(),
                            )
                        } else {
                            (None, None)
                        };
                        (cur, obj.sleeper_count() as i64, cw, wt)
                    }
                    _ => (None, -1, None, None),
                }
            } else {
                (None, -1, None, None)
            };
            let lost = cur.is_some_and(|cur| {
                // A 32-bit sleep compares only the low half; setup_sleep_word32 truncates the same
                // way, so the judgment here must too or a benign high bit reads as a lost wake.
                let armed = if sw_meta & 1 != 0 {
                    sw_val & 0xffff_ffff
                } else {
                    sw_val
                };
                let eq = cur == armed;
                let asleep_pred = if sw_meta & 2 != 0 { !eq } else { eq };
                !asleep_pred
                    && thread.sync_links.is_linked()
                    && thread.get_state() == ExecutionState::Sleeping
            });
            emerglogln!(
                "  thread {} ({}) '{}': {:?} sctx {} in_user {} must_exit {} ip {:x} word {}+{:x} wv {:x} wvok {} av {:x} m {} slprs {} cw {} wt {} lost {} fl {:x} crit {} gen {} cs {} cg {} rq {} rg {} | sync {} pager {} memwait {} mutex {} condvar {} requeue {} suspend {} sched {} timed {}",
                thread.id(),
                thread.objid(),
                core::str::from_utf8(&namebuf[..namelen]).unwrap_or("?"),
                thread.get_state(),
                thread.active_sctx_id(),
                thread.is_in_user(),
                thread.must_exit(),
                thread.read_ip(),
                sw_obj,
                sw_off,
                cur.unwrap_or(u64::MAX),
                cur.is_some(),
                sw_val,
                sw_meta,
                slprs,
                cw.map(|v| v as i64).unwrap_or(-1),
                wt.map(|v| v as i64).unwrap_or(-1),
                lost,
                thread.flags.load(Ordering::Relaxed),
                thread.is_critical(),
                thread.sync_sleep_gen(),
                thread.sync_consumer[0].load(Ordering::Relaxed),
                thread.sync_consumer[1].load(Ordering::Relaxed),
                thread.requeue_event[0].load(Ordering::Relaxed),
                thread.requeue_event[1].load(Ordering::Relaxed),
                thread.sync_links.is_linked(),
                thread.pager_link.is_linked(),
                thread.memwait_link.is_linked(),
                thread.mutex_link.is_linked(),
                thread.condvar_link.is_linked(),
                thread.requeue_link.is_linked(),
                thread.suspend_link.is_linked(),
                thread.sched_link.is_linked(),
                thread.has_timed_wait(),
            );
        }
    }
    // Never silently: a truncated table that looks complete is how a missing thread becomes a
    // thread nobody was looking for.
    if untabled > 0 {
        emerglogln!(
            "  ... and {} further threads the table could not hold (cap {})",
            untabled,
            HANG_TABLE_MAX_THREADS
        );
    }
    // The counters are otherwise only printed at shutdown, which a wedged boot never reaches -- so
    // the one run in a thousand that actually hangs is the one that reports none of them. Printing
    // them here costs a few lines on a boot already in trouble, and makes a hung transcript
    // self-contained.
    locktrack::diag::print_counters(true);
}

pub fn check_orphan_threads() {
    //#[cfg(debug_assertions)]
    with_all_threads(|at| {
        for thread in at.iter() {
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
            // A force-exit that never landed. The shutdown hang leaves exactly this behind -- a
            // thread carrying THREAD_MUST_EXIT that never reaches a poll point -- but the
            // transcript has never said *where* it is stuck, only that the system went
            // quiet after `ChangeState` printed. The linkage above already answers
            // that: whichever of these is true names the wait it is parked on
            // (sync_linked = a thread-sync sleep, pager_linked = an inflight pager
            // reply, memwait_linked = the memory tracker, mutex/condvar = a kernel
            // lock), and all-false with state Running is the orphan case instead.
            if thread.must_exit() && thread.get_state() != ExecutionState::Exited {
                let scans = thread.stuck_exit_scans.fetch_add(1, Ordering::Relaxed) + 1;
                if scans == STUCK_EXIT_SCANS {
                    emerglogln!(
                        "thread {} ({}) has an undelivered force-exit after {} scans: sctx_ok {} (active {}), state {:?}, critical {}, mutex_count {}, mutex_linked {}, condvar_linked {}, sync_linked {}, pager_linked {}, memwait_linked {}, requeue_linked {}, suspend_linked {}, sched_linked {}, timed_wait {}, active_running {}",
                        thread.id(),
                        thread.objid(),
                        scans,
                        thread.exit_sctx_ok(),
                        thread.active_sctx_id(),
                        thread.get_state(),
                        thread.is_critical(),
                        thread.get_mutex_count(),
                        is_mutex_linked,
                        is_condvar_linked,
                        is_sync_linked,
                        is_pager_linked,
                        is_memwait_linked,
                        is_requeue_linked,
                        is_suspend_linked,
                        is_sched_linked,
                        is_timed_wait,
                        is_active_running,
                    );
                    thread.print_locks();
                }
            } else {
                thread.stuck_exit_scans.store(0, Ordering::Relaxed);
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
