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
    syscall::{sys_object_stat, sys_thread_self_id},
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{
    error::{ArgumentError, NamingError, ObjectError, TwzError},
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

// Temporary instrumentation for the File::open latency hunt (pagerperf.md). Accumulators only --
// no TLS, no allocation -- because most of `cross_compartment_entry` runs in the zero-TLS window.
mod entrystats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static ATTACH: AtomicU64 = AtomicU64::new(0);
    static SETSCTX: AtomicU64 = AtomicU64::new(0);
    static LOCK: AtomicU64 = AtomicU64::new(0);
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    pub fn set_pending(attach: u64, setsctx: u64) {
        ATTACH.fetch_add(attach, Ordering::Relaxed);
        SETSCTX.fetch_add(setsctx, Ordering::Relaxed);
    }

    pub fn record(lock: u64, total: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let l = LOCK.fetch_add(lock, Ordering::Relaxed) + lock;
        let t = TOTAL.fetch_add(total, Ordering::Relaxed) + total;
        if n.is_power_of_two() {
            twizzler_abi::klog_println!(
                "ENTRYSTATS {} entries: settls+attach {} us, set-sctx {} us, mgr-lock {} us, \
                 total {} us",
                n,
                ATTACH.load(Ordering::Relaxed) / 1000,
                SETSCTX.load(Ordering::Relaxed) / 1000,
                l / 1000,
                t / 1000,
            );
        }
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
    /// # The zero-TLS window
    ///
    /// The thread arrives holding the *caller* compartment's thread pointer, which must not be used
    /// for anything -- reading through it is a cross-compartment access -- so it is zeroed
    /// immediately and stays zero until this compartment's region is installed below.
    ///
    /// **Nothing called between those two `sys_thread_settls` calls may touch thread-local storage,
    /// directly or indirectly.** With the thread pointer at zero, `mov {}, fs:0` -- how the control
    /// block is found -- reads linear address 0 and faults; there is no null to test for, because
    /// the read *is* the fault. That rules out the global allocator, which reads the control block
    /// on every allocation, and so rules out any collection that allocates through it.
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
        twizzler_abi::syscall::sys_thread_settls(0);
        if OUR_RUNTIME.is_monitor().is_some() {
            twizzler_abi::syscall::sys_thread_set_active_sctx_id(0.into()).inspect_err(|e| {
                twizzler_abi::klog_println!("failed to set sctx: {}", e);
            })?;
        } else {
            let _ = twizzler_abi::syscall::sys_sctx_attach(monitor_api::get_comp_config().sctx)
                .inspect_err(|e| {
                    if !matches!(e, TwzError::Naming(NamingError::AlreadyBound)) {
                        twizzler_abi::klog_println!("failed to attach sctx: {}", e);
                    }
                });
            let t_attach = OUR_RUNTIME.get_monotonic();
            twizzler_abi::syscall::sys_thread_set_active_sctx_id(
                monitor_api::get_comp_config().sctx,
            )
            .inspect_err(|e| {
                twizzler_abi::klog_println!("failed to set-a sctx: {}", e);
            })?;
            let t_setsctx = OUR_RUNTIME.get_monotonic();
            entrystats::set_pending(
                t_attach.saturating_sub(t0).as_nanos() as u64,
                t_setsctx.saturating_sub(t_attach).as_nanos() as u64,
            );
        }
        let t_pre_lock = OUR_RUNTIME.get_monotonic();
        let mut inner = THREAD_MGR.inner.lock();

        if let Some(ct) = inner
            .cross_threads
            .get(&twizzler_abi::syscall::sys_thread_self_id())
        {
            twizzler_abi::syscall::sys_thread_settls(ct.tls as u64);
            entrystats::record(
                OUR_RUNTIME
                    .get_monotonic()
                    .saturating_sub(t_pre_lock)
                    .as_nanos() as u64,
                OUR_RUNTIME.get_monotonic().saturating_sub(t0).as_nanos() as u64,
            );
            return Ok(());
        }

        let id = inner.next_id().freeze();
        drop(inner);
        let (tls, layout, alloc_base) = TLS_GEN_MGR
            .lock()
            .get_next_tls_info(None, || RuntimeThreadControl::new(id))
            .unwrap();
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
        libc_init_tcb(tls);

        with_current_thread(|cur| {
            cur.flags.fetch_or(THREAD_STARTED, Ordering::SeqCst);
        });

        // Mapped here, while the thread is demonstrably alive (it is us), so that the GC scan can
        // read its execution state later instead of inferring death from a failed stat.
        let self_id = twizzler_abi::syscall::sys_thread_self_id();

        THREAD_MGR.inner.lock().cross_threads.insert(
            self_id,
            CrossThread {
                tls,
                layout,
                id,
                alloc_base,
            },
        );
        Ok(())
    }

    pub(super) fn impl_spawn(
        &self,
        mut args: twizzler_rt_abi::thread::ThreadSpawnArgs,
    ) -> Result<(u32, *mut c_void)> {
        if args.stack_size < 1024 * 1024 * 8 {
            args.stack_size = 1024 * 1024 * 8;
        }
        // Box this up so we can pass it to the new thread.
        let args = Box::new(args);
        let (tls, tls_layout, tls_alloc_base) = TLS_GEN_MGR
            .lock()
            .get_next_tls_info(None, || RuntimeThreadControl::new(0))
            .unwrap();

        if OUR_RUNTIME.state().contains(RuntimeState::READY) {
            libc_init_tcb(tls);
        }
        let stack_raw = unsafe {
            OUR_RUNTIME
                .alloc_zeroed(Layout::from_size_align(args.stack_size, MIN_STACK_ALIGN).unwrap())
        } as usize;

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

        let thid: ObjID = {
            let res: Result<_> =
                monitor_api::monitor_rt_spawn_thread(new_args, tls as usize, stack_raw);

            match res {
                Ok(id) => ObjID::from(id),
                Err(e) => return Err(e),
            }
        };

        // Nothing past this point may return `Err`. `monitor_rt_spawn_thread` above has already
        // started the thread, and it is running on `arg_raw` -- a pointer to std's `ThreadInit`,
        // which std frees the moment `twz_rt_spawn_thread` reports failure. The thread then
        // dereferences that freed box and calls through a `dyn FnOnce` vtable pointer read back
        // out of recycled heap, i.e. it jumps to a heap address and refaults there forever.
        //
        // The map fails when the thread has already exited and the monitor has deleted its repr,
        // which is a race we lose legitimately for short-lived threads, not an error. A missing
        // handle just means the state has to be read from the object's existence instead.
        let thread_repr_obj = self
            .map_object(thid, MapFlags::READ | MapFlags::WRITE)
            .inspect_err(|e| tracing::debug!("failed to map repr of new thread {}: {}", thid, e))
            .ok();

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
