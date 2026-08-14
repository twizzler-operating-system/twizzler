//! Thread management routines, including spawn and join.

use std::{
    alloc::{GlobalAlloc, Layout},
    collections::BTreeMap,
    ffi::c_void,
    sync::atomic::Ordering,
};

use monitor_api::{RuntimeThreadControl, Tcb, THREAD_STARTED};
use tracing::trace;
use twizzler_abi::{
    object::{ObjID, NULLPAGE_SIZE},
    simple_mutex::Mutex,
    syscall::{sys_object_stat, sys_thread_self_id, SctxSwitchFlags},
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{
    error::{ArgumentError, ObjectError, TwzError},
    object::MapFlags,
    thread::ThreadSpawnArgs,
    Result,
};

use super::internal::InternalThread;
use crate::{
    runtime::{
        alloc::{LocalAllocator, LOCAL_ALLOCATOR},
        thread::{
            libc_init_tcb,
            tcb::{trampoline, TLS_GEN_MGR},
            with_current_thread, MIN_STACK_ALIGN, THREAD_MGR,
        },
        ReferenceRuntime, OUR_RUNTIME,
    },
    RuntimeState,
};

/// Zero the whole stack at spawn, the way this used to. `false` zeroes only [`STACK_TOP_ZERO`] at
/// the top; see there for why the rest does not need it.
const ZERO_WHOLE_STACK: bool = false;

/// How much of the top of a new stack to zero.
///
/// Nothing reads stack memory before writing it: the kernel only computes the initial `rsp` as
/// `base + size - 8` and jumps (`arch::thread::new_stack_top`), the upcall path writes its frame
/// downward from the top, and the entry function writes its frame before reading it. So the bulk of
/// an 8 MiB stack is memset for nothing -- and with ferroc's `IS_ZEROED = false` over reusable
/// chunks, that memset is real work on every spawn.
///
/// What does need defined contents is the 8-byte slot `rsp` points at on entry, which is popped as
/// a return address if the entry function ever returns: zero faults cleanly at IP 0, while stale
/// heap bytes could be a live code address. A page rather than a word, so an unwinder walking a
/// frame past the entry frame also finds zeroes.
const STACK_TOP_ZERO: usize = 0x1000;

/// Floor on a spawned thread's stack.
///
/// This used to be 8 MiB, which is four times what libstd asks for and is charged to every thread
/// whether or not it wants it -- a caller requesting 64 KiB got 8 MiB. Note that it does *not* land
/// in ferroc's `Large` class: that tops out at `LARGE_MAX` = 1,966,080 bytes (SLAB_SIZE 4 MiB,
/// SHARD_SIZE 64 KiB), so 2 MiB still routes to the huge path and takes a chunk from the base
/// allocator. Dropping to 1,966,080 would land it in `Large`.
const MIN_STACK_SIZE: usize = 2 * 1024 * 1024;

// Temporary instrumentation for the File::open latency hunt (pagerperf.md). Accumulators only --
// no TLS, no allocation -- because the cold half of `cross_compartment_entry` runs in the zero-TLS
// window.

mod entrystats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static COLD: AtomicU64 = AtomicU64::new(0);
    static SWITCH: AtomicU64 = AtomicU64::new(0);
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    pub fn record(switch: u64, total: u64, cold: bool) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let s = SWITCH.fetch_add(switch, Ordering::Relaxed) + switch;
        let t = TOTAL.fetch_add(total, Ordering::Relaxed) + total;
        if cold {
            COLD.fetch_add(1, Ordering::Relaxed);
        }
        if secgate::statcadence::report_now(n) {
            secgate::statlog::record(
                "ENTRYSTA",
                n,
                &[COLD.load(Ordering::Relaxed), s / 1000, t / 1000],
            );
        }
    }
}

/// Per-spawn phase timings from the calling compartment (`SPAWNRT`), independent of the global
/// `STATS_ON` so a spawn-path run does not turn on every other counter in the tree.
///
/// The open question this is here to answer is the `tls` phase (`sysperf.md` lead 4): the monitor's
/// super-TLS region is recycled and prebuilt, but the *caller's* region is still a fresh
/// `LOCAL_ALLOCATOR.alloc` of the compartment's TLS template on every spawn, freed again in
/// `InternalThread::drop`. It measured 452 us back when a spawn was 14.9 ms, and has not been
/// measured since the rounds that took a spawn to ~277 us.
mod spawnstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Switch for the spawn-path counters only.
    pub(super) const ON: bool = false;

    static N: AtomicU64 = AtomicU64::new(0);

    pub(super) fn record(tls: u64, stack: u64, gate: u64, map: u64) {
        if !ON {
            return;
        }
        let n = N.fetch_add(1, Ordering::Relaxed) + 1;
        secgate::statlog::record_on(ON, "SPAWNRT", n, &[tls, stack, gate, map]);
    }

    pub(super) fn since(start: std::time::Instant) -> u64 {
        if !ON {
            return 0;
        }
        start.elapsed().as_nanos() as u64
    }
}

/// Recycled thread stacks.
///
/// A spawn's `stack` phase measures 21-40 us, for what is nominally one allocation and a 4 KiB
/// memset -- because a 2 MiB request clears ferroc's `LARGE_MAX` and takes a fresh span from the
/// base allocator, whose pages nothing has touched yet. The memset at the top of the stack is
/// then a page fault, and the thread's first frames fault again.
///
/// Handing the stack to the next spawn instead keeps those pages mapped. The safety requirement
/// is exactly the one [`InternalThread::drop`] already meets by calling `dealloc` there -- the
/// thread is gone and nothing else names its stack -- so recycling adds no new obligation.
pub(super) mod stackpool {
    use std::sync::Mutex;

    /// Stacks held before further returns go back to the allocator. Each pins its whole size,
    /// which is at least [`super::MIN_STACK_SIZE`].
    const MAX: usize = 8;

    /// A/B switch for measuring what recycling is worth; `false` restores the old behavior of
    /// allocating and freeing each stack.
    const RECYCLE: bool = true;

    static POOL: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

    /// A recycled stack of exactly `size` bytes, if one is waiting.
    pub(in crate::runtime) fn take(size: usize) -> Option<usize> {
        if !RECYCLE {
            return None;
        }
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        let idx = pool.iter().position(|(_, s)| *s == size)?;
        Some(pool.swap_remove(idx).0)
    }

    /// Returns false if the pool is full and the caller should free the stack itself.
    pub(in crate::runtime) fn put(addr: usize, size: usize) -> bool {
        if !RECYCLE {
            return false;
        }
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() >= MAX {
            return false;
        }
        // Reserve once, so a return from a thread-exit path never grows this Vec under the lock.
        if pool.capacity() == 0 {
            pool.reserve(MAX);
        }
        pool.push((addr, size));
        true
    }
}

pub(crate) struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

impl ThreadManager {
    pub(super) const fn new() -> Self {
        Self {
            inner: Mutex::new(ThreadManagerInner::new()),
        }
    }

    pub fn with_internal<R, F: FnOnce(&InternalThread) -> R>(&self, id: u32, f: F) -> Option<R> {
        let inner = self.inner.lock();
        Some(f(inner.all_threads.get(&id)?))
    }

    pub fn gc(&self) {
        let mut inner = self.inner.lock();
        inner.scan_for_exited_cross();
        inner.scan_for_exited_except(0);
        inner.do_thread_gc();
    }
}

struct CrossThread {
    tls: *mut Tcb<RuntimeThreadControl>,
    layout: Layout,
    id: twizzler_rt_abi::thread::ThreadId,
    alloc_base: *mut u8,
}

/// Whether the thread owning this repr object is gone.
///
/// This has to be answerable without calling into the monitor: the only way to read a thread's
/// `ExecutionState` is to map its repr, and outside the monitor `map_object` is a gate call --
/// which cannot be made from `cross_compartment_entry` (it wedges the compartment) and so
/// cannot be made while the thread is known alive. What is left is the existence of the
/// repr object, which is destroyed only once the thread is gone.
///
/// The narrowing that matters: **only** `NoSuchObject` counts as death. The previous test was
/// `sys_object_stat(id).is_err()`, so a permissions failure, or any future error, silently
/// freed a live thread's TLS -- and a thread that keeps running on a freed region reads its
/// control block back as zero once the allocator reuses the memory, which is a fault at a
/// small negative address inside whatever thread-local it touches next.
fn repr_is_gone(id: ObjID) -> bool {
    match sys_object_stat(id) {
        Err(TwzError::Object(ObjectError::NoSuchObject)) => true,
        Err(_) | Ok(_) => false,
    }
}

extern "C" {
    fn std_handle_thread_exit(
        id: twizzler_rt_abi::thread::ThreadId,
        my_tp: *mut u8,
        their_tp: *mut u8,
    );
    fn __mlibc_handle_thread_exit(pointer: *mut u8, ret_val: i32);
}

impl Drop for CrossThread {
    fn drop(&mut self) {
        let tls = unsafe { dynlink::tls::get_current_thread_control_block::<()>() };
        let my_tp = tls as *mut u8;
        unsafe {
            std_handle_thread_exit(self.id, my_tp, self.tls.cast::<u8>());
            __mlibc_handle_thread_exit(self.tls.cast(), 0);
            LOCAL_ALLOCATOR.dealloc(self.alloc_base, self.layout);
        }
    }
}

struct ThreadManagerInner {
    all_threads: BTreeMap<u32, InternalThread, &'static LocalAllocator>,
    cross_threads: BTreeMap<ObjID, CrossThread, &'static LocalAllocator>,
    // Threads that have exited, but we haven't cleaned up yet.
    to_cleanup: Vec<InternalThread>,
    // Basic unique-ID system.
    id_stack: Vec<u32>,
    next_id: u32,
}

unsafe impl Send for ThreadManager {}
unsafe impl Sync for ThreadManager {}

impl ThreadManagerInner {
    const fn new() -> Self {
        Self {
            next_id: 2, // 0 is reserved, 1 is the core thread.
            all_threads: BTreeMap::new_in(&LOCAL_ALLOCATOR),
            to_cleanup: vec![],
            id_stack: vec![],
            cross_threads: BTreeMap::new_in(&LOCAL_ALLOCATOR),
        }
    }

    fn prep_cleanup(&mut self, id: u32) {
        if let Some(th) = self.all_threads.remove(&id) {
            self.to_cleanup.push(th);
        }
    }

    fn do_thread_gc(&mut self) {
        trace!(
            "starting thread GC round with {} dead threads",
            self.to_cleanup.len()
        );
        for th in self.to_cleanup.drain(..) {
            drop(th)
        }
        self.scan_for_exited_cross();
    }

    fn scan_for_exited_cross(&mut self) {
        for (_, th) in self.cross_threads.extract_if(.., |id, _| repr_is_gone(*id)) {
            drop(th);
        }
    }

    fn scan_for_exited_except(&mut self, id: u32) {
        for (_, th) in self.all_threads.extract_if(.., |_, th| {
            th.id != id
                && match th.repr() {
                    Some(repr) => repr.get_state() == ExecutionState::Exited,
                    // No mapping to read the state from; fall back to the existence of the repr
                    // object, which outlives the thread.
                    None => repr_is_gone(th.objid()),
                }
        }) {
            trace!("found orphaned thread {}", th.id);
            self.to_cleanup.push(th);
        }
    }

    fn next_id(&mut self) -> IdDropper<'_> {
        let raw = self.id_stack.pop().unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        IdDropper { tm: self, id: raw }
    }

    fn release_id(&mut self, id: u32) {
        self.id_stack.push(id)
    }
}

// Makes spawn easier to read, as it'll auto-cleanup IDs on failure.
struct IdDropper<'a> {
    tm: &'a mut ThreadManagerInner,
    id: u32,
}

impl<'a> IdDropper<'a> {
    fn freeze(mut self) -> u32 {
        let id = self.id;
        self.id = 0;
        id
    }
}

impl<'a> Drop for IdDropper<'a> {
    fn drop(&mut self) {
        if self.id != 0 {
            self.tm.release_id(self.id)
        }
    }
}

impl ReferenceRuntime {
    pub fn init_core_thread(
        &self,
        tls: *mut Tcb<RuntimeThreadControl>,
        tls_alloc_base: *mut u8,
        tls_layout: Layout,
    ) {
        let thid = sys_thread_self_id();
        let thread_repr_obj = self
            .map_object(thid, MapFlags::READ | MapFlags::WRITE)
            .unwrap();
        (unsafe { &mut *tls }).runtime_data.set_id(1);
        let thread = InternalThread::new(
            Some(thread_repr_obj),
            thid,
            0,
            0,
            0,
            1,
            tls,
            tls_alloc_base,
            tls_layout,
        );

        THREAD_MGR
            .inner
            .lock()
            .all_threads
            .insert(thread.id, thread);
    }

    // Temporary instrumentation for the File::open latency hunt (pagerperf.md).

    /// Re-point this thread at *this* compartment's TLS on entry through a gate.
    ///
    /// One syscall does the whole context switch: it attaches this compartment's security context
    /// if the thread is not attached yet, switches to it, and -- because the kernel tracks the user
    /// thread pointer per (thread, context) -- swaps in whatever thread pointer this thread last
    /// used *here*, reporting it back. So the caller's thread pointer is gone before this returns,
    /// without a `settls` to zero it, and a nonzero return means %fs already points at a TLS region
    /// this compartment built on an earlier entry.
    ///
    /// # The zero-TLS window
    ///
    /// A zero return means this thread has never run in this compartment, and everything below
    /// runs with a null thread pointer until `sys_thread_settls` installs a region.
    ///
    /// **Nothing in that window may touch thread-local storage, directly or indirectly.** With the
    /// thread pointer at zero, `mov {}, fs:0` -- how the control block is found -- reads linear
    /// address 0 and faults; there is no null to test for, because the read *is* the fault. That
    /// rules out the global allocator, which reads the control block on every allocation, and so
    /// rules out any collection that allocates through it.
    ///
    /// What the window is allowed: syscalls, `simple_mutex::Mutex` (thread-sync, no TLS),
    /// `klog_println!`, and `LOCAL_ALLOCATOR`'s methods, which reach talc directly. `TLS_GEN_MGR`'s
    /// map is allocator-parameterized for exactly this reason, and `next_id()` is `freeze`d so its
    /// `Drop` cannot push to a `Vec`.
    pub fn cross_compartment_entry(&self) -> Result<()> {
        // Temporary instrumentation for the File::open latency hunt (pagerperf.md).
        // Times phases with OUR_RUNTIME.get_monotonic() directly rather than Instant, to stay
        // clear of anything that might touch TLS inside the zero-TLS window.
        let t0 = OUR_RUNTIME.get_monotonic();
        // The monitor is instance zero, and asking `get_comp_config()` for that would be a gate
        // call back into itself.
        let sctx = if OUR_RUNTIME.is_monitor().is_some() {
            ObjID::new(0)
        } else {
            monitor_api::get_comp_config().sctx
        };
        let tp =
            twizzler_abi::syscall::sys_thread_set_active_sctx_id_ext(sctx, SctxSwitchFlags::ATTACH)
                .inspect_err(|e| {
                    twizzler_abi::klog_println!("failed to enter sctx {}: {}", sctx, e);
                })?;
        let t_switch = OUR_RUNTIME.get_monotonic();
        let switch_ns = t_switch.saturating_sub(t0).as_nanos() as u64;
        if tp != 0 {
            entrystats::record(
                switch_ns,
                OUR_RUNTIME.get_monotonic().saturating_sub(t0).as_nanos() as u64,
                false,
            );
            return Ok(());
        }

        // Cold: this thread has no TLS in this compartment. Everything from here to the `settls`
        // below is inside the zero-TLS window described above.
        let self_id = twizzler_abi::syscall::sys_thread_self_id();
        let mut inner = THREAD_MGR.inner.lock();
        if let Some(ct) = inner.cross_threads.get(&self_id) {
            // A region we built on an earlier entry that the kernel has no record of -- it should
            // have handed it back above. Reinstalling it is both correct and cheaper than leaking
            // a second region for the same thread.
            twizzler_abi::syscall::sys_thread_settls(ct.tls as u64);
            entrystats::record(
                switch_ns,
                OUR_RUNTIME.get_monotonic().saturating_sub(t0).as_nanos() as u64,
                true,
            );
            return Ok(());
        }

        let id = inner.next_id().freeze();
        drop(inner);
        let (tls, layout, alloc_base) = TLS_GEN_MGR
            .lock()
            .get_next_tls_info(None, || RuntimeThreadControl::new(id))
            .unwrap();
        // Ends the zero-TLS window, and registers the pointer with the kernel: it is saved against
        // this compartment's context on the way out, so the next entry takes the warm path.
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
        libc_init_tcb(tls);

        with_current_thread(|cur| {
            cur.flags.fetch_or(THREAD_STARTED, Ordering::SeqCst);
        });

        THREAD_MGR.inner.lock().cross_threads.insert(
            self_id,
            CrossThread {
                tls,
                layout,
                id,
                alloc_base,
            },
        );
        entrystats::record(
            switch_ns,
            OUR_RUNTIME.get_monotonic().saturating_sub(t0).as_nanos() as u64,
            true,
        );
        Ok(())
    }

    pub(super) fn impl_spawn(
        &self,
        mut args: twizzler_rt_abi::thread::ThreadSpawnArgs,
    ) -> Result<(u32, *mut c_void)> {
        if args.stack_size < MIN_STACK_SIZE {
            args.stack_size = MIN_STACK_SIZE;
        }
        // Box this up so we can pass it to the new thread.
        let args = Box::new(args);
        let t_tls = std::time::Instant::now();
        let (tls, tls_layout, tls_alloc_base) = TLS_GEN_MGR
            .lock()
            .get_next_tls_info(None, || RuntimeThreadControl::new(0))
            .unwrap();

        if OUR_RUNTIME.state().contains(RuntimeState::READY) {
            libc_init_tcb(tls);
        }
        let tls_ns = spawnstats::since(t_tls);
        let t_stack = std::time::Instant::now();
        let stack_raw = unsafe {
            let layout = Layout::from_size_align(args.stack_size, MIN_STACK_ALIGN).unwrap();
            if ZERO_WHOLE_STACK {
                OUR_RUNTIME.alloc_zeroed(layout)
            } else {
                let p = stackpool::take(args.stack_size)
                    .map(|p| p as *mut u8)
                    .unwrap_or_else(|| OUR_RUNTIME.alloc(layout));
                if !p.is_null() {
                    let from = args.stack_size.saturating_sub(STACK_TOP_ZERO);
                    core::ptr::write_bytes(p.add(from), 0, args.stack_size - from);
                }
                p
            }
        } as usize;
        let stack_ns = spawnstats::since(t_stack);

        // Take the thread management lock, so that when the new thread starts we cannot observe
        // that thread running without the management data being recorded.
        let mut inner = THREAD_MGR.inner.lock();
        let id = inner.next_id();

        // Set the thread's ID. After this the TCB is ready.
        unsafe {
            tls.as_mut().unwrap().runtime_data.set_id(id.id);
        }

        let stack_size = args.stack_size;
        let arg_raw = Box::into_raw(args) as usize;

        tracing::debug!(
            "spawning thread {} with stack {:x}, entry {:x}, and TLS {:p}",
            id.id,
            stack_raw,
            trampoline as *const () as usize,
            tls,
        );

        let new_args = ThreadSpawnArgs {
            stack_size,
            start: trampoline as *const () as usize,
            arg: arg_raw,
        };

        let t_gate = std::time::Instant::now();
        let thid: ObjID = {
            let res: Result<_> =
                monitor_api::monitor_rt_spawn_thread(new_args, tls as usize, stack_raw);

            match res {
                Ok(id) => ObjID::from(id),
                Err(e) => return Err(e),
            }
        };
        let gate_ns = spawnstats::since(t_gate);

        // Nothing past this point may return `Err`. `monitor_rt_spawn_thread` above has already
        // started the thread, and it is running on `arg_raw` -- a pointer to std's `ThreadInit`,
        // which std frees the moment `twz_rt_spawn_thread` reports failure. The thread then
        // dereferences that freed box and calls through a `dyn FnOnce` vtable pointer read back
        // out of recycled heap, i.e. it jumps to a heap address and refaults there forever.
        //
        // The map fails when the thread has already exited and the monitor has deleted its repr,
        // which is a race we lose legitimately for short-lived threads, not an error. A missing
        // handle just means the state has to be read from the object's existence instead.
        let t_map = std::time::Instant::now();
        let thread_repr_obj = self
            .map_object(thid, MapFlags::READ | MapFlags::WRITE)
            .inspect_err(|e| tracing::debug!("failed to map repr of new thread {}: {}", thid, e))
            .ok();
        spawnstats::record(tls_ns, stack_ns, gate_ns, spawnstats::since(t_map));

        let thread = InternalThread::new(
            thread_repr_obj,
            thid,
            stack_raw,
            stack_size,
            arg_raw,
            id.freeze(),
            tls,
            tls_alloc_base,
            tls_layout,
        );
        let id = thread.id;
        inner.all_threads.insert(thread.id, thread);

        Ok((id, tls.cast()))
    }

    pub(super) fn impl_join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<()> {
        let start = std::time::Instant::now();
        // Usually one pass: the thread has a repr handle and we go straight to waiting on it. The
        // loop is for the thread whose repr could not be mapped at spawn time, where there is no
        // wait word to sleep on -- either the object is gone (the thread has exited, so the join
        // is already satisfied) or the map failed transiently and is worth retrying.
        let repr = loop {
            let repr_id = {
                let mut inner = THREAD_MGR.inner.lock();
                inner.scan_for_exited_except(id);
                let thread = inner
                    .all_threads
                    .get(&id)
                    .ok_or(TwzError::Argument(ArgumentError::BadHandle))?;
                match thread.repr_handle() {
                    Some(repr) => break repr.clone(),
                    None => {
                        let repr_id = thread.objid();
                        if repr_is_gone(repr_id) {
                            inner.prep_cleanup(id);
                            inner.do_thread_gc();
                            return Ok(());
                        }
                        repr_id
                    }
                }
            };

            if let Ok(repr) = self.map_object(repr_id, MapFlags::READ | MapFlags::WRITE) {
                if let Some(thread) = THREAD_MGR.inner.lock().all_threads.get_mut(&id) {
                    thread.set_repr_handle(repr.clone());
                }
                break repr;
            }
            if timeout.is_some_and(|timeout| start.elapsed() >= timeout) {
                return Err(TwzError::TIMED_OUT);
            }
            self.sleep(std::time::Duration::from_millis(1));
        };
        let timeout = timeout.map(|timeout| timeout.saturating_sub(start.elapsed()));
        let base =
            unsafe { (repr.start().add(NULLPAGE_SIZE) as *const ThreadRepr).as_ref() }.unwrap();
        loop {
            let (state, _code) = base
                .wait_until(ExecutionState::Exited, timeout)
                .ok_or(TwzError::TIMED_OUT)?;
            if state == ExecutionState::Exited {
                let mut inner = THREAD_MGR.inner.lock();
                inner.prep_cleanup(id);
                inner.do_thread_gc();
                return Ok(());
            }
        }
    }
}
