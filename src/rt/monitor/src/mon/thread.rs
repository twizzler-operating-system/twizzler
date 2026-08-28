use std::{
    collections::HashMap,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};

use dynlink::{
    compartment::{Compartment, MONITOR_COMPARTMENT_ID},
    tls::TlsRegion,
};
use monitor_api::{RuntimeThreadControl, ThreadMgrStats, MONITOR_INSTANCE_ID};
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{
        sys_object_ctrl, sys_spawn, sys_thread_exit, DeleteFlags, ObjectControlCmd,
        ThreadSyncSleep, UpcallTargetSpawnOption,
    },
    thread::{ExecutionState, ThreadRepr},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_rt_abi::{
    error::{GenericError, TwzError},
    object::{MapFlags, ObjID},
};

use super::{
    get_monitor,
    space::{MapHandle, MapInfo},
};
use crate::mon::space::Space;

mod cleaner;
pub(crate) use cleaner::ThreadCleaner;

/// Everything a freshly spawned monitor thread needs, as plain data.
///
/// This replaces a `Box<dyn FnOnce()>` trampoline. That box was never freed: it is invoked through
/// `FnOnce for Box<F>`, which deallocates *after* the call returns, and every monitor thread's body
/// ends in `sys_thread_resume_from_upcall` (`-> !`). Boxing less does not help either -- a free
/// emitted before a diverging call is sunk past it and deleted as unreachable (verified in the
/// disassembly). The only robust fix is to allocate nothing, which is what this is for: it is
/// written into the base of the thread's own super stack, which the monitor already owns and
/// reclaims on reap.
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct EntryArgs {
    pub instance: ObjID,
    pub stack_ptr: usize,
    pub stack_size: usize,
    pub thread_ptr: usize,
    pub entry: usize,
    pub arg: usize,
    pub suspend: bool,
}

/// Stack size for the supervisor upcall stack.
pub const SUPER_UPCALL_STACK_SIZE: usize = 2 * 1024 * 1024; // 2MB
/// Zero the whole super stack at spawn, the way this used to. A/B against `false`, which zeroes
/// only the top.
const ZERO_WHOLE_SUPER_STACK: bool = false;
/// How much of the top of the super stack to zero when `ZERO_WHOLE_SUPER_STACK` is false.
const SUPER_STACK_TOP_ZERO: usize = 0x1000;
/// Default stack size for the user stack.
pub const DEFAULT_STACK_SIZE: usize = 2 * 1024 * 1024; // 2MB
/// Stack minimium alignment.
pub const STACK_SIZE_MIN_ALIGN: usize = 0x1000; // 4K

/// Per-spawn phase timings (`SPAWNMON`/`SPAWNMNP`), independent of the global `STATS_ON`.
///
/// The spawn path has no cheaper instrument: rounds of this work have shown boot wall clock cannot
/// resolve ~30 us x ~128 spawns, so an A/B needs these in the build. See `sysperf.md` round 5.
pub(crate) mod spawnstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Switch for the spawn-path counters only.
    pub(crate) const ON: bool = false;

    static N: AtomicU64 = AtomicU64::new(0);

    /// Phase timings [`super::ThreadMgr::finish_spawn`] fills in, since it is the only place that
    /// can bracket them.
    #[derive(Default)]
    pub(crate) struct Phases {
        pub stack: u64,
        pub sys_spawn: u64,
        pub reprmap: u64,
    }

    /// One record per spawn: all phases in ns, tagged by whether the TLS region came from the
    /// prebuilt pool (`SPAWNMNP`) or was built under the monitor's lock collection (`SPAWNMON`).
    pub(crate) fn record(
        pooled: bool,
        lockwait: u64,
        tls: u64,
        stack: u64,
        sys_spawn: u64,
        reprmap: u64,
        register: u64,
    ) {
        if !ON {
            return;
        }
        let n = N.fetch_add(1, Ordering::Relaxed) + 1;
        secgate::statlog::record_on(
            ON,
            if pooled { "SPAWNMNP" } else { "SPAWNMON" },
            n,
            &[lockwait, tls, stack, sys_spawn, reprmap, register],
        );
    }

    /// Nanoseconds since `start`. Userspace `Instant::now` memoizes the tickrate, so this is an
    /// rdtsc and a multiply, not a syscall.
    pub(crate) fn since(start: std::time::Instant) -> u64 {
        if !ON {
            return 0;
        }
        start.elapsed().as_nanos() as u64
    }
}

/// Supervisor stacks and TLS regions, recycled across spawns.
///
/// The monitor's heap is append-only: twz-rt routes every allocation made with
/// `RuntimeState::IS_MONITOR` set to the *early* talc, and that talc's `dealloc` is a no-op
/// because `early_allocs_frozen` is only ever set on the path the monitor's own allocations
/// return before reaching. So a dropped super stack was not reused -- each spawn took another
/// 2 MiB span of heap nothing had ever touched, and the first write to it was a page fault that
/// no later spawn could amortize. That is the ~40 us `stack` phase, and 2 MiB of growth per
/// thread the monitor ever spawns.
///
/// Recycling fixes both without touching the allocator: a returned stack is already mapped, so
/// the next spawn's write to it faults nothing, and the monitor's footprint stops tracking the
/// number of threads it has ever started. It also retires the TLS-region half of leak M1
/// (`mleaks.md`) for every thread that exits cleanly.
mod pool {
    use std::{
        alloc::Layout,
        mem::MaybeUninit,
        ptr::NonNull,
        sync::{Mutex, MutexGuard},
    };

    use dynlink::tls::TlsRegion;

    /// Entries of each kind held before further returns are dropped instead.
    ///
    /// Dropping is what happens today for every entry, so overflowing this is exactly the old
    /// behavior rather than a new leak -- but bound it anyway, since a compartment teardown can
    /// retire many threads at once and each entry pins 2 MiB.
    const MAX: usize = 32;

    /// A/B switch for measuring what recycling is worth; `false` restores the old behavior, in
    /// which every returned stack and TLS region was abandoned.
    const RECYCLE: bool = true;

    struct Tls {
        base: NonNull<u8>,
        layout: Layout,
    }

    // Safety: a pooled region has no owner -- it is placed here only after its thread has been
    // observed `Exited`, and handed out to exactly one spawn.
    unsafe impl Send for Tls {}

    struct Pool {
        stacks: Vec<Box<[MaybeUninit<u8>]>>,
        tls: Vec<Tls>,
    }

    static POOL: Mutex<Pool> = Mutex::new(Pool {
        stacks: Vec::new(),
        tls: Vec::new(),
    });

    fn lock() -> MutexGuard<'static, Pool> {
        POOL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A recycled supervisor stack of exactly `len` bytes, if one is waiting.
    pub(super) fn take_stack(len: usize) -> Option<Box<[MaybeUninit<u8>]>> {
        if !RECYCLE {
            return None;
        }
        let mut pool = lock();
        let idx = pool.stacks.iter().position(|s| s.len() == len)?;
        Some(pool.stacks.swap_remove(idx))
    }

    pub(super) fn put_stack(stack: Box<[MaybeUninit<u8>]>) {
        if !RECYCLE {
            return;
        }
        let mut pool = lock();
        if pool.stacks.len() < MAX {
            // Reserve once, so a return from a thread-exit path never grows this Vec under the
            // lock.
            pool.stacks.reserve(MAX);
            pool.stacks.push(stack);
        }
    }

    /// A recycled TLS allocation for exactly `layout`, zeroed.
    ///
    /// Zeroing is not optional: `copy_in_module` writes only `template_filesz` bytes per module,
    /// so the `.tbss` tail of every module is whatever the allocation already held -- which for a
    /// fresh `alloc_zeroed` is zero and for a recycled region is the previous thread's data.
    pub(super) fn take_tls(layout: Layout) -> Option<NonNull<u8>> {
        if !RECYCLE {
            return None;
        }
        let base = {
            let mut pool = lock();
            let idx = pool.tls.iter().position(|t| t.layout == layout)?;
            pool.tls.swap_remove(idx).base
        };
        // Safety: the region is `layout.size()` bytes and has no other owner.
        unsafe { std::ptr::write_bytes(base.as_ptr(), 0, layout.size()) };
        Some(base)
    }

    pub(super) fn put_tls(region: &TlsRegion) {
        let Some(base) = NonNull::new(region.alloc_base()) else {
            return;
        };
        let layout = region.alloc_layout();
        if RECYCLE {
            let mut pool = lock();
            if pool.tls.len() < MAX {
                pool.tls.reserve(MAX);
                pool.tls.push(Tls { base, layout });
                return;
            }
        }
        // Terminal owner. Neither `Tls` nor `TlsRegion` owns the block -- both are descriptors --
        // so a region that lands in no pool must be freed here or it is leaked outright. The
        // stack path never had this bug because `put_stack` takes an owning `Box`, which frees
        // itself when the pool is full; this path takes a `&TlsRegion` and dropped it on the floor.
        // Outside the pool lock: `dealloc` reaches the monitor's allocator, which takes its own.
        unsafe { std::alloc::dealloc(base.as_ptr(), layout) };
    }
}

/// Super-TLS regions built *before* the spawn that uses them.
///
/// [`pool`] above recycles the allocation; this recycles the work. `build_tls_region` needs
/// `&mut Compartment` out of the monitor's dynlink lock, and happylock hands the monitor's five
/// locks out as one collection -- so a spawn that builds its own region waits on whatever holds
/// them, which during a compartment load is up to 12 ms (`sysperf.md` lead 5c). Nothing about that
/// wait is inherent: the region does not depend on the spawn, only on the compartment's TLS
/// template, so it can be built at a time nobody is waiting.
///
/// The cleaner thread builds them (it is the monitor's existing background worker) and a spawn
/// pops one under a plain mutex. An empty pool falls back to building inline, so this is an
/// optimization with a floor, not a new requirement.
pub(crate) mod readypool {
    use std::sync::{Mutex, MutexGuard};

    use dynlink::tls::TlsRegion;

    /// Regions kept ready. Each pins one TLS allocation.
    const MAX: usize = 4;
    /// Refill when the pool is at or below this. Below `MAX` so a burst of spawns does not have to
    /// empty the pool completely before a refill starts.
    const LOW: usize = 2;

    /// A/B switch: `false` sends every spawn down the inline (lock-collection) path.
    const PREBUILD: bool = true;

    /// A region parked for the next spawn. Every entry was built from `Pool::gen`, which is what
    /// makes that one field enough to say whether any of them are still valid.
    struct Ready {
        region: TlsRegion,
    }

    // Safety: same argument as `pool::Tls` -- a region parked here has no owner, and is handed to
    // exactly one spawn.
    unsafe impl Send for Ready {}

    struct Pool {
        ready: Vec<Ready>,
        /// The generation the entries were built from, and the one the next refill compares
        /// against.
        gen: u64,
    }

    static POOL: Mutex<Pool> = Mutex::new(Pool {
        ready: Vec::new(),
        gen: 0,
    });

    fn lock() -> MutexGuard<'static, Pool> {
        POOL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A prebuilt region, if one is waiting. The TCB still needs its id set.
    pub(crate) fn take() -> Option<TlsRegion> {
        if !PREBUILD {
            return None;
        }
        lock().ready.pop().map(|r| r.region)
    }

    /// How many more regions the pool wants; 0 when it is stocked.
    pub(super) fn wanted() -> usize {
        if !PREBUILD {
            return 0;
        }
        let pool = lock();
        if pool.ready.len() > LOW {
            0
        } else {
            MAX - pool.ready.len()
        }
    }

    /// Park a freshly built region. Discards it if the pool is full.
    ///
    /// A `gen` that differs from what the pool holds empties it: those regions were built against
    /// a template that no longer describes the compartment. This can only happen if a library is
    /// loaded into the *monitor's own* dynlink compartment, which nothing in-tree does after boot
    /// -- `load_library_by_name` loads into the caller's compartment -- but the check costs one
    /// comparison and the alternative is handing a thread TLS storage that is missing a module.
    pub(super) fn put(region: TlsRegion, gen: u64) {
        if !PREBUILD {
            return;
        }
        // Anything that does not end up parked here is handed to `pool::put_tls`, which either
        // recycles the allocation or frees it. Dropping a `TlsRegion` leaks its block.
        let mut displaced = Some(region);
        let mut stale = None;
        {
            let mut pool = lock();
            if pool.gen != gen {
                // One per call, so the drain never runs a free under this lock. When the last one
                // is out the generation advances and the next call parks normally.
                stale = pool.ready.pop().map(|r| r.region);
                if stale.is_none() {
                    pool.gen = gen;
                }
            }
            if pool.gen == gen && pool.ready.len() < MAX {
                pool.ready.reserve(MAX);
                // Unwrap-Ok: `displaced` is `Some` until this line, which runs at most once.
                pool.ready.push(Ready {
                    region: displaced.take().unwrap(),
                });
            }
        }
        if let Some(r) = stale {
            super::pool::put_tls(&r);
        }
        if let Some(r) = displaced {
            super::pool::put_tls(&r);
        }
    }
}

/// Build one super-TLS region out of the monitor's dynlink compartment.
///
/// Allocation comes from [`pool`] when it has one of the right layout, so a region built here
/// usually writes into pages that are already mapped.
fn build_super_tls(monitor_dynlink_comp: &mut Compartment) -> Result<TlsRegion, TwzError> {
    monitor_dynlink_comp
        .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
            pool::take_tls(layout).or_else(|| NonNull::new(std::alloc::alloc_zeroed(layout)))
        })
        .map_err(|_| GenericError::Internal.into())
}

/// Stamp a thread id into a region's control block. The last thing a region needs before a thread
/// can run on it.
pub(crate) fn init_super_tcb(super_tls: &TlsRegion, super_tid: u32) {
    unsafe {
        let tcb = super_tls.get_thread_control_block::<RuntimeThreadControl>();
        (*tcb).runtime_data.set_id(super_tid);
    }
}

/// Top the prebuilt TLS pool back up. Called from the cleaner thread, which is the only place in
/// the monitor that can take the lock collection without anyone waiting on the result.
pub(crate) fn refill_ready_tls() {
    let wanted = readypool::wanted();
    if wanted == 0 {
        return;
    }
    // The cleaner does not hold the key here, but bail rather than panic if that ever changes:
    // failing to prebuild costs a spawn the fallback path, and nothing else.
    let Ok(key) = super::reentrant_key() else {
        return;
    };
    let monitor = get_monitor();
    let locks = &mut *crate::lockdiag::watched(monitor.locks.lock(key));
    let Ok(comp) = locks.2.get_compartment_mut(MONITOR_COMPARTMENT_ID) else {
        return;
    };
    for _ in 0..wanted {
        match build_super_tls(comp) {
            Ok(region) => {
                let gen = region.gen;
                readypool::put(region, gen);
            }
            Err(e) => {
                tracing::warn!("failed to prebuild a super TLS region: {}", e);
                break;
            }
        }
    }
}

/// Manages all threads owned by the monitor. Typically, this is all threads.
/// Threads are spawned here and tracked in the background by a [cleaner::ThreadCleaner]. The thread
/// cleaner detects when a thread has exited and performs any final thread cleanup logic.
pub struct ThreadMgr {
    all: HashMap<ObjID, ManagedThread>,
    cleaner: OnceLock<cleaner::ThreadCleaner>,
    next_id: u32,
    id_stack: Vec<u32>,
}

impl Default for ThreadMgr {
    fn default() -> Self {
        Self {
            all: HashMap::default(),
            cleaner: OnceLock::new(),
            next_id: 1,
            id_stack: Vec::new(),
        }
    }
}

struct IdDropper<'a> {
    mgr: &'a mut ThreadMgr,
    id: u32,
}

impl<'a> IdDropper<'a> {
    pub fn freeze(self) -> u32 {
        let id = self.id;
        std::mem::forget(self);
        id
    }
}

impl<'a> Drop for IdDropper<'a> {
    fn drop(&mut self) {
        self.mgr.release_super_tid(self.id);
    }
}

impl ThreadMgr {
    pub(super) fn set_cleaner(&mut self, cleaner: cleaner::ThreadCleaner) {
        self.cleaner.set(cleaner).ok().unwrap();
    }

    fn next_super_tid(&mut self) -> IdDropper<'_> {
        let id = self.id_stack.pop().unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        IdDropper { mgr: self, id }
    }

    pub(super) fn release_super_tid(&mut self, id: u32) {
        self.id_stack.push(id);
    }

    /// Every live thread spawned for `instance`.
    pub fn threads_of(&self, instance: ObjID) -> Vec<ObjID> {
        self.all
            .values()
            .filter(|t| t.instance == instance)
            .map(|t| t.id)
            .collect()
    }

    fn do_remove(&mut self, thread: &ManagedThread) {
        self.all.remove(&thread.id);
        self.release_super_tid(thread.super_tid);
        if let Some(cleaner) = self.cleaner.get() {
            cleaner.untrack(thread.id);
        }
    }

    pub fn stat(&self) -> ThreadMgrStats {
        ThreadMgrStats {
            nr_threads: self.all.len(),
        }
    }

    unsafe fn spawn_thread(
        start: usize,
        super_stack_start: usize,
        super_thread_pointer: usize,
        arg: usize,
        self_ctx: ObjID,
    ) -> Result<ObjID, TwzError> {
        let mut upcall_target = UpcallTarget::new(
            None,
            Some(twizzler_rt_abi::arch::__twz_rt_upcall_entry),
            super_stack_start,
            SUPER_UPCALL_STACK_SIZE,
            super_thread_pointer,
            MONITOR_INSTANCE_ID,
            self_ctx,
            [UpcallOptions {
                flags: UpcallFlags::empty(),
                mode: UpcallMode::CallSuper,
            }; UpcallInfo::NR_UPCALLS],
        );

        let mb = &mut upcall_target.options[UpcallInfo::Mailbox(0).number()];
        mb.mode = UpcallMode::CallSelf;

        sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
            entry: start,
            stack_base: super_stack_start,
            stack_size: SUPER_UPCALL_STACK_SIZE,
            tls: super_thread_pointer,
            arg,
            flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
            vm_context_handle: None,
            upcall_target: UpcallTargetSpawnOption::SetTo(upcall_target),
        })
    }

    /// The part of a spawn that needs the monitor's locks: a TLS region out of the monitor's
    /// dynlink compartment, and an id.
    ///
    /// Split from [`Self::finish_spawn`] because everything else a spawn does -- allocating the
    /// super stack, `sys_spawn`, mapping the repr -- needs none of them, and holding the whole
    /// monitor lock collection across all of it serialized every spawn against every other monitor
    /// operation in the system. Measured at 3.7 ms per spawn under that lock.
    pub(super) fn prep_spawn(
        &mut self,
        monitor_dynlink_comp: &mut Compartment,
    ) -> Result<(TlsRegion, u32), TwzError> {
        let super_tls = build_super_tls(monitor_dynlink_comp)?;
        let super_tid = self.take_super_tid();
        init_super_tcb(&super_tls, super_tid);
        Ok((super_tls, super_tid))
    }

    /// An id for a new thread, without the rest of [`Self::prep_spawn`].
    ///
    /// The pooled path needs this and nothing else from the monitor's state.
    pub(super) fn take_super_tid(&mut self) -> u32 {
        self.next_super_tid().freeze()
    }

    /// The part of a spawn that holds no monitor lock. See [`Self::prep_spawn`].
    pub(super) fn finish_spawn(
        super_tls: TlsRegion,
        super_tid: u32,
        start: unsafe extern "C" fn(usize) -> !,
        args: EntryArgs,
        main_thread_comp: Option<ObjID>,
        instance: ObjID,
        phases: &mut spawnstats::Phases,
    ) -> Result<ManagedThread, TwzError> {
        let super_thread_pointer = super_tls.get_thread_pointer_value();
        let t_stack = std::time::Instant::now();
        let mut super_stack = if ZERO_WHOLE_SUPER_STACK {
            Box::new_zeroed_slice(SUPER_UPCALL_STACK_SIZE)
        } else {
            let mut stack = pool::take_stack(SUPER_UPCALL_STACK_SIZE)
                .unwrap_or_else(|| Box::new_uninit_slice(SUPER_UPCALL_STACK_SIZE));
            // The kernel writes the upcall frame downward from the top of this stack and reads
            // nothing from it, so only the top needs defined contents. See `STACK_TOP_ZERO` in
            // twz-rt's thread manager for the full argument; the same one applies here, and this
            // 8 MiB memset was the other half of what a spawn was paying.
            let from = SUPER_UPCALL_STACK_SIZE.saturating_sub(SUPER_STACK_TOP_ZERO);
            unsafe {
                core::ptr::write_bytes(
                    stack.as_mut_ptr().add(from).cast::<u8>(),
                    0,
                    SUPER_UPCALL_STACK_SIZE - from,
                );
            }
            stack
        };
        phases.stack = spawnstats::since(t_stack);
        // The thread's args go at the *base* of its own super stack, and the base pointer handed to
        // `spawn_thread` is unchanged -- no reserve, so nothing has to agree with us about where
        // the stack top is. The stack grows down from base + SUPER_UPCALL_STACK_SIZE, so
        // reaching these bytes is already an overflow, and the entry copies them to a local
        // at depth ~0 before anything else runs.
        //
        // Written *after* the branch above, so both positions of `ZERO_WHOLE_SUPER_STACK` work by
        // construction, and unconditionally, because `pool::take_stack` hands back a recycled stack
        // holding the previous thread's bytes. Unaligned because `Box<[MaybeUninit<u8>]>` is align
        // 1 by type while `ObjID` is align 16 -- true in practice, not guaranteed by
        // anything.
        let arg = super_stack.as_ptr() as usize;
        unsafe { core::ptr::write_unaligned(super_stack.as_mut_ptr().cast::<EntryArgs>(), args) };
        let t_spawn = std::time::Instant::now();
        let id = unsafe {
            Self::spawn_thread(
                start as *const () as usize,
                super_stack.as_ptr() as usize,
                super_thread_pointer,
                arg,
                instance,
            )?
        };
        phases.sys_spawn = spawnstats::since(t_spawn);
        let t_reprmap = std::time::Instant::now();
        // We own this repr object from here: the kernel no longer deletes it when the thread dies
        // (see Thread::drop), so every path out of here has to either hand it to a
        // ManagedThreadRepr, which deletes it on drop, or delete it directly.
        //
        // Mapped writable even though we only read it, so this shares one `Space` entry -- and so
        // one slot -- with the mapping `spawn_compartment_thread` hands the owning compartment,
        // which asks for READ | WRITE. `MapInfo` is keyed by flags as well as id, so asking for
        // READ here would map the same object twice.
        let repr = match Space::map(
            &get_monitor().space,
            MapInfo {
                id,
                flags: MapFlags::READ | MapFlags::WRITE,
            },
            ObjID::new(0),
        ) {
            Ok(repr) => repr,
            Err(e) => {
                tracing::error!(
                    "failed to map repr object {} of newly spawned thread: {}",
                    id,
                    e
                );
                // `spawn_thread` above has already started the thread, and it is running on this
                // stack. Dropping the Box would hand that memory back to the allocator to be
                // reused underneath a live thread, so leak it instead. (`super_tls` has no `Drop`,
                // so it leaks either way.)
                std::mem::forget(super_stack);
                delete_repr(id);
                return Err(e);
            }
        };
        phases.reprmap = spawnstats::since(t_reprmap);
        Ok(Arc::new(ManagedThreadInner {
            id,
            super_tid,
            repr: ManagedThreadRepr::new(repr),
            super_stack: Some(super_stack),
            super_tls,
            main_thread_comp,
            instance,
        }))
    }

    /// Start a thread with the monitor's locks already held.
    ///
    /// The compartment loader is inside them for its own reasons, so it uses this rather than
    /// `Monitor::start_thread`'s three-phase version. Compartment loading is rare; ordinary thread
    /// spawns should not come through here.
    pub fn start_thread(
        &mut self,
        monitor_dynlink_comp: &mut Compartment,
        start: unsafe extern "C" fn(usize) -> !,
        args: EntryArgs,
        main_thread_comp: Option<ObjID>,
        instance: ObjID,
    ) -> Result<ManagedThread, TwzError> {
        let (super_tls, super_tid) = self.prep_spawn(monitor_dynlink_comp)?;
        let mut phases = spawnstats::Phases::default();
        match Self::finish_spawn(
            super_tls,
            super_tid,
            start,
            args,
            main_thread_comp,
            instance,
            &mut phases,
        ) {
            Ok(mt) => {
                self.register(&mt);
                Ok(mt)
            }
            Err(e) => {
                self.release_super_tid(super_tid);
                Err(e)
            }
        }
    }

    /// Record a thread built by [`Self::finish_spawn`], and start tracking it for cleanup.
    pub(super) fn register(&mut self, mt: &ManagedThread) {
        self.all.insert(mt.id, mt.clone());
        if let Some(cleaner) = self.cleaner.get() {
            cleaner.track(mt.clone());
        }
    }
}

/// Internal managed thread data.
pub struct ManagedThreadInner {
    /// The ID of the thread.
    pub id: ObjID,
    pub super_tid: u32,
    /// The thread repr.
    pub(crate) repr: ManagedThreadRepr,
    /// `None` only after [`Drop`] has handed it back to the [`pool`].
    super_stack: Option<Box<[MaybeUninit<u8>]>>,
    super_tls: TlsRegion,
    pub main_thread_comp: Option<ObjID>,
    /// The compartment this thread was spawned for. Recorded so teardown can find every thread of
    /// a compartment: `RunComp::per_thread` only holds threads that have called a gate needing
    /// the simple buffer, which is a subset, and killing a subset is what leaves the survivors
    /// blocked on a socket engine whose poll thread is gone.
    pub instance: ObjID,
}

impl ManagedThreadInner {
    /// Check if this thread has exited.
    pub fn has_exited(&self) -> bool {
        self.repr.get_repr().get_state() == ExecutionState::Exited
    }

    /// Create a ThreadSyncSleep that will wait until the thread has exited.
    pub fn waitable_until_exit(&self) -> ThreadSyncSleep {
        self.repr.get_repr().waitable(ExecutionState::Exited)
    }
}

// Safety: TlsRegion is not changed, and points to only globally- and permanently-allocated data.
unsafe impl Send for ManagedThreadInner {}
unsafe impl Sync for ManagedThreadInner {}

impl core::fmt::Debug for ManagedThreadInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ManagedThread({})", self.id)
    }
}

impl Drop for ManagedThreadInner {
    fn drop(&mut self) {
        tracing::trace!("dropping ManagedThread {}", self.id);
        // Recycle only what is provably unowned. A live thread still runs on this stack -- and
        // takes upcalls on it -- so handing it to the next spawn would put two threads on one
        // stack. Dropping instead is exactly what happened before there was a pool: the monitor's
        // allocator does not reclaim it either way, so the unsafe case costs nothing new.
        if !self.has_exited() {
            tracing::warn!(
                "last reference to still-running thread {} dropped; leaking its supervisor stack",
                self.id
            );
            return;
        }
        if let Some(stack) = self.super_stack.take() {
            pool::put_stack(stack);
        }
        pool::put_tls(&self.super_tls);
    }
}

/// A thread managed by the monitor.
pub type ManagedThread = Arc<ManagedThreadInner>;

/// Delete a thread repr object. The kernel creates these but leaves their lifetime to whoever
/// called `sys_spawn`, so the monitor has to do this explicitly or they accumulate forever.
fn delete_repr(id: ObjID) {
    if let Err(e) = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0) {
        tracing::warn!("failed to delete thread repr object {}: {}", id, e);
    }
}

/// An owned handle to a thread's repr object.
pub(crate) struct ManagedThreadRepr {
    handle: MapHandle,
}

impl ManagedThreadRepr {
    fn new(handle: MapHandle) -> Self {
        Self { handle }
    }

    /// Get the thread representation structure for the associated thread.
    pub fn get_repr(&self) -> &ThreadRepr {
        let addr = self.handle.addrs().start + NULLPAGE_SIZE;
        unsafe { (addr as *const ThreadRepr).as_ref().unwrap() }
    }
}

impl Drop for ManagedThreadRepr {
    fn drop(&mut self) {
        tracing::trace!("dropping ManagedThreadRepr for {}", self.handle.id());
        // Deliberately on handle drop rather than where the cleaner detects the exit, so that
        // every path that lets go of a thread -- cleaner reap, ThreadMgr removal, teardown --
        // releases the object. The unmap is deferred to the Unmapper, but ordering does not
        // matter: Delete only marks, and the kernel reaps once the last mapping is gone.
        delete_repr(self.handle.id());
    }
}
