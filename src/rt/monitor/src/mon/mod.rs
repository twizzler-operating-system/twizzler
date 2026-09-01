use std::{
    ptr::NonNull,
    sync::{Mutex, OnceLock},
};

use compartment::{
    StackObject, COMP_DESTRUCTED, COMP_EXITED, COMP_IS_BINARY, COMP_READY, COMP_STARTED,
    COMP_THREAD_CAN_EXIT,
};
use dynlink::compartment::MONITOR_COMPARTMENT_ID;
use happylock::{LockCollection, RwLock, ThreadKey};
use monitor_api::{
    CompartmentFlags, MonitorCompControlCmd, PostSignalFlags, RuntimeThreadControl,
    SharedCompConfig, TlsTemplateInfo, MONITOR_INSTANCE_ID,
};
use secgate::util::HandleMgr;
use space::Space;
use talc::{ErrOnOom, Talc};
use thread::DEFAULT_STACK_SIZE;
use twizzler_abi::{
    syscall::{sys_thread_change_state, sys_thread_exit, sys_thread_send_message},
    upcall::{ResumeFlags, UpcallData, UpcallFrame},
    write_note,
};
use twizzler_rt_abi::{
    error::{GenericError, TwzError},
    object::{MapFlags, ObjID},
    thread::ThreadSpawnArgs,
};

use self::{
    compartment::{CompConfigObject, CompartmentHandle, RunComp},
    space::{MapHandle, MapInfo, Unmapper},
    thread::{ManagedThread, ThreadCleaner, ThreadMgr},
};
use crate::init::InitDynlinkContext;

pub(crate) mod compartment;
pub mod library;
pub(crate) mod space;
pub mod stat;
pub(crate) mod thread;

/// Take the calling thread's happylock key, failing instead of panicking if it already holds one.
///
/// happylock hands each thread exactly one key, so the monitor's locks are not re-entrant. A gate
/// call arriving from a thread that already holds the key has to fail: the case that matters is a
/// monitor panic, whose backtrace walk re-enters through `get_image_info` -> the library and
/// compartment getters below. Unwrapping there turns a diagnosable panic into
/// "panicked while processing panic. aborting." with no stack at all.
pub(crate) fn reentrant_key() -> Result<ThreadKey, TwzError> {
    ThreadKey::get().ok_or(GenericError::WouldBlock.into())
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md): splits the monitor's
// side of a map gate into the space lock plus `sys_object_map`, reaching the compartment, and
// recording the mapping in it.
mod monmapstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Master switch for the clock reads; see `space::spacesplit::TIMING`, same reasoning. The
    /// three `Instant::now()` pairs below sit on every object map in the system.
    pub const TIMING: bool = false;

    /// `Instant::now()`, if [`TIMING`] is on.
    #[inline(always)]
    pub fn t0() -> Option<std::time::Instant> {
        TIMING.then(std::time::Instant::now)
    }

    /// Nanoseconds since `t`, or 0 when [`TIMING`] is off.
    #[inline(always)]
    pub fn ns(t: Option<std::time::Instant>) -> u64 {
        t.map_or(0, |t| t.elapsed().as_nanos() as u64)
    }

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static SPACE: AtomicU64 = AtomicU64::new(0);
    static MGR: AtomicU64 = AtomicU64::new(0);
    static REC: AtomicU64 = AtomicU64::new(0);

    pub fn record(space: u64, mgr: u64, rec: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let s = SPACE.fetch_add(space, Ordering::Relaxed) + space;
        let m = MGR.fetch_add(mgr, Ordering::Relaxed) + mgr;
        let r = REC.fetch_add(rec, Ordering::Relaxed) + rec;
        if TIMING && secgate::statcadence::report_now(n) {
            secgate::statlog::record("MONMAPST", n, &[s / 1000, m / 1000, r / 1000]);
        }
    }
}

// Temporary: which monitor entry points actually reach a thread's simple buffer, and how often.
//
// `get_thread_simple_buffer` turned out to be cold -- 8 calls over a whole boot, because the client
// caches the id in a `#[thread_local] OnceCell` (`monitor_api::lazy_sb`). These counters say which
// of the *other* per-thread-buffer paths carry real traffic, so a workload can be chosen before
// anything is measured against it. One `klog_println!` per report: it does not line-buffer, so a
// multi-line report from several threads interleaves into garbage.
pub(crate) mod ptstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Entry points that reach `RunComp::get_per_thread`, directly or via
    /// `read_thread_simple_buffer`.
    #[derive(Clone, Copy, Debug)]
    pub enum Site {
        GetSb = 0,
        CompInfo = 1,
        LibInfo = 2,
        LoadLib = 3,
        LookupSym = 4,
        Spawn = 5,
        // The five that shared `read_sb` in the first pass, which was 66% of all traffic. Split
        // because "narrow the lock further" and "stop making the call" are different fixes and
        // the aggregate could not tell them apart.
        GateAddr = 6,
        LookupComp = 7,
        LoadComp = 8,
        LibNameMap = 9,
        LibNameUnmap = 10,
        // Not buffer users: the inline gates. Counted so the collapse of
        // `gate_addr`/`lookup_comp` is visibly a move, not a disappearance.
        GateAddrInline = 11,
        LookupCompInline = 12,
    }
    const NR_SITES: usize = 13;
    const NAMES: [&str; NR_SITES] = [
        "get_sb",
        "comp_info",
        "lib_info",
        "load_lib",
        "lookup_sym",
        "spawn",
        "gate_addr",
        "lookup_comp",
        "load_comp",
        "libname_map",
        "libname_unmap",
        "gate_addr_INL",
        "lookup_comp_INL",
    ];

    static SITES: [AtomicU64; NR_SITES] = [const { AtomicU64::new(0) }; NR_SITES];
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    pub fn record(site: Site) {
        SITES[site as usize].fetch_add(1, Ordering::Relaxed);
        let n = TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
        if !secgate::statcadence::report_now(n) {
            return;
        }
        let mut line = format!("PTSTATS {} per-thread-buffer calls:", n);
        for (i, name) in NAMES.iter().enumerate() {
            line.push_str(&format!(" {} {},", name, SITES[i].load(Ordering::Relaxed)));
        }
        twizzler_abi::klog_println!("{}", line);
    }
}

/// A security monitor instance. All monitor logic is implemented as methods for this type.
/// We split the state into the following components: 'space', managing the virtual memory space and
/// mapping objects, 'thread_mgr', which manages all threads owned by the monitor (typically, all
/// threads started by compartments), 'compartments', which manages compartment state, and
/// 'dynlink', which contains the dynamic linker state. The unmapper allows for background unmapping
/// and cleanup of objects and handles. There are also two hangle managers, for the monitor to hand
/// out handles to libraries and compartments to callers.
pub struct Monitor {
    locks: LockCollection<MonitorLocks<'static>>,
    /// The two locks a compartment-handle lookup actually touches.
    ///
    /// `lookup_compartment_id`, `lookup_compartment_named` and `get_compartment_handle` used
    /// [`Self::locks`] for this, which takes all five -- so a `comps` lookup and a handle insert
    /// blocked every spawn (which takes `thread_mgr`) and every library operation for as long as
    /// they held it. Measured at 502 holds over a millisecond in one boot, 724 ms of collection
    /// time, none of it needing `thread_mgr`, `dynlink`, or the library handles (`sysperf.md`
    /// round 8).
    ///
    /// A second collection is sound here for the same reason the first one is: happylock hands
    /// each thread a single key and `lock` consumes it, so no thread can hold two collections and
    /// no cycle exists to order.
    comp_lookup: LockCollection<CompLookupLocks<'static>>,
    unmapper: OnceLock<Unmapper>,
    /// Management of address space.
    pub space: &'static Mutex<space::Space>,
    /// Management of all threads.
    pub thread_mgr: &'static RwLock<thread::ThreadMgr>,
    /// Management of compartments.
    pub comp_mgr: &'static RwLock<compartment::CompartmentMgr>,
    /// Dynamic linker state.
    pub dynlink: &'static RwLock<&'static mut dynlink::context::Context>,
    /// Open handles to libraries.
    pub library_handles: &'static RwLock<HandleMgr<library::LibraryHandle>>,
    /// Open handles to compartments.
    pub _compartment_handles: &'static RwLock<HandleMgr<CompartmentHandle>>,
}

// We allow locking individually, using eg mon.space.write(key), or locking the collection for more
// complex operations that touch multiple pieces of state.
/// The subset [`Monitor::comp_lookup`] takes. Order matches its position in [`MonitorLocks`].
type CompLookupLocks<'a> = (
    &'a RwLock<compartment::CompartmentMgr>,
    &'a RwLock<HandleMgr<CompartmentHandle>>,
);

type MonitorLocks<'a> = (
    &'a RwLock<thread::ThreadMgr>,
    &'a RwLock<compartment::CompartmentMgr>,
    &'a RwLock<&'static mut dynlink::context::Context>,
    &'a RwLock<HandleMgr<library::LibraryHandle>>,
    &'a RwLock<HandleMgr<CompartmentHandle>>,
);

/// Entry point for an ordinary compartment thread spawn. Plain function, no closure, no allocation.
///
/// Deliberately *not* merged with `comp_main_entry`: this one guards on a zero instance and ignores
/// a failed attach, where the compartment-main path attaches unconditionally and unwraps. Unifying
/// them would silently change whether a failed `sys_sctx_attach` panics a monitor thread.
unsafe extern "C" fn comp_spawn_entry(args: usize) -> ! {
    let a = unsafe { core::ptr::read_unaligned(args as *const thread::EntryArgs) };
    if a.instance.raw() != 0 {
        let _ = twizzler_abi::syscall::sys_sctx_attach(a.instance);
    }
    let frame = UpcallFrame::new_entry_frame(
        a.stack_ptr,
        a.stack_size,
        a.thread_ptr,
        a.instance,
        a.entry,
        a.arg,
    );
    unsafe { twizzler_abi::syscall::sys_thread_resume_from_upcall(&frame, ResumeFlags::empty()) }
}

impl Monitor {
    /// Start the background threads for the monitor instance. Must be done only once the monitor
    /// has been initialized.
    pub fn start_background_threads(&self) {
        crate::lockdiag::start_watchdog();
        let cleaner = ThreadCleaner::new();
        self.unmapper.set(Unmapper::new()).ok().unwrap();
        self.thread_mgr
            .write(ThreadKey::get().unwrap())
            .set_cleaner(cleaner);
    }

    /// Build a new monitor state from the initial dynamic linker context.
    pub fn new(init: InitDynlinkContext) -> Self {
        let mut comp_mgr = compartment::CompartmentMgr::default();
        let space = Mutex::new(space::Space::default());

        let ctx = init.get_safe_context();
        // Build our TLS region, and create a template for the monitor compartment.
        let super_tls = ctx
            .get_compartment_mut(MONITOR_COMPARTMENT_ID)
            .unwrap()
            .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
                NonNull::new(std::alloc::alloc_zeroed(layout))
            })
            .unwrap();
        let template: &'static TlsTemplateInfo = Box::leak(Box::new(super_tls.into()));

        // Set up the monitor's compartment.
        let monitor_scc = SharedCompConfig::new(
            MONITOR_INSTANCE_ID,
            template as *const _ as *mut _,
            monitor_api::CompartmentLoaderConfig::default(),
        );
        let cc_handle = Space::safe_create_and_map_runtime_object(
            &space,
            MONITOR_INSTANCE_ID,
            MapFlags::READ | MapFlags::WRITE,
        )
        .unwrap();
        write_note!(cc_handle.id(), "monitor-scc");
        let stack_handle = Space::safe_create_and_map_runtime_object(
            &space,
            MONITOR_INSTANCE_ID,
            MapFlags::READ | MapFlags::WRITE,
        )
        .unwrap();
        write_note!(stack_handle.id(), "monitor-stack");

        let comp_config = CompConfigObject::new(cc_handle, monitor_scc);
        let mut alloc = Talc::new(ErrOnOom);
        unsafe { alloc.claim(comp_config.alloc_span()).unwrap() };
        comp_mgr.insert(RunComp::new(
            MONITOR_INSTANCE_ID,
            MONITOR_INSTANCE_ID,
            "monitor".to_string(),
            MONITOR_COMPARTMENT_ID,
            vec![],
            comp_config,
            (CompartmentFlags::READY | CompartmentFlags::STARTED).bits(),
            StackObject::new(stack_handle, DEFAULT_STACK_SIZE).unwrap(),
            0, /* doesn't matter -- we won't be starting a main thread for this compartment in
                * the normal way */
            0,
            &[],
            false,
            None,
            alloc,
        ));

        // Allocate and leak all the locks (they are global and eternal, so we can do this to safely
        // and correctly get &'static lifetime)
        let space = Box::leak(Box::new(space));
        let thread_mgr = Box::leak(Box::new(RwLock::new(thread::ThreadMgr::default())));
        let comp_mgr = Box::leak(Box::new(RwLock::new(comp_mgr)));
        let dynlink = Box::leak(Box::new(RwLock::new(ctx)));
        let library_handles = Box::leak(Box::new(RwLock::new(HandleMgr::new(None))));
        let compartment_handles = Box::leak(Box::new(RwLock::new(HandleMgr::new(None))));

        // Okay to call try_new here, since it's not many locks and only happens once.
        Self {
            locks: LockCollection::try_new((
                &*thread_mgr,
                &*comp_mgr,
                &*dynlink,
                &*library_handles,
                &*compartment_handles,
            ))
            .unwrap(),
            comp_lookup: LockCollection::try_new((&*comp_mgr, &*compartment_handles)).unwrap(),
            unmapper: OnceLock::new(),
            space,
            thread_mgr,
            comp_mgr,
            dynlink,
            library_handles,
            _compartment_handles: compartment_handles,
        }
    }

    /// Start a managed monitor thread.
    ///
    /// Three phases, and only the first and last hold a monitor lock. The middle one -- allocating
    /// the super stack, `sys_spawn`, and mapping the new thread's repr -- needs nothing from the
    /// monitor's state, and it is where essentially all of a spawn's time goes. Holding the whole
    /// lock collection across it, as this used to, meant spawns could not overlap each other and
    /// every unrelated monitor operation in the system queued behind them.
    #[tracing::instrument(skip(self, args), level = tracing::Level::DEBUG)]
    pub fn start_thread(
        &self,
        instance: ObjID,
        start: unsafe extern "C" fn(usize) -> !,
        args: thread::EntryArgs,
    ) -> Result<ManagedThread, TwzError> {
        // Two ways to get a TLS region. The prebuilt one is the point of the pool: it needs no
        // dynlink state, so this takes `thread_mgr` alone for an id instead of the whole lock
        // collection, and stops queueing behind every unrelated monitor operation. Falling back
        // costs what a spawn always cost.
        let t_lock = std::time::Instant::now();
        let (super_tls, super_tid, pooled, lockwait, tls_ns) = match thread::readypool::take() {
            Some(super_tls) => {
                let key = ThreadKey::get().unwrap();
                let mut tmgr = crate::lockdiag::watched(self.thread_mgr.write(key));
                let lockwait = thread::spawnstats::since(t_lock);
                let super_tid = tmgr.take_super_tid();
                drop(tmgr);
                thread::init_super_tcb(&super_tls, super_tid);
                (super_tls, super_tid, true, lockwait, 0)
            }
            None => {
                let key = ThreadKey::get().unwrap();
                let locks = &mut *crate::lockdiag::watched(self.locks.lock(key));
                let lockwait = thread::spawnstats::since(t_lock);
                let t_tls = std::time::Instant::now();
                let monitor_dynlink_comp =
                    locks.2.get_compartment_mut(MONITOR_COMPARTMENT_ID).unwrap();
                let (super_tls, super_tid) = locks.0.prep_spawn(monitor_dynlink_comp)?;
                (
                    super_tls,
                    super_tid,
                    false,
                    lockwait,
                    thread::spawnstats::since(t_tls),
                )
            }
        };

        let mut phases = thread::spawnstats::Phases::default();
        let mt = ThreadMgr::finish_spawn(
            super_tls,
            super_tid,
            start,
            args,
            None,
            instance,
            &mut phases,
        );

        let t_reg = std::time::Instant::now();
        let key = ThreadKey::get().unwrap();
        let mut tmgr = crate::lockdiag::watched(self.thread_mgr.write(key));
        match mt {
            Ok(mt) => {
                tmgr.register(&mt);
                drop(tmgr);
                thread::spawnstats::record(
                    pooled,
                    lockwait,
                    tls_ns,
                    phases.stack,
                    phases.sys_spawn,
                    phases.reprmap,
                    thread::spawnstats::since(t_reg),
                );
                Ok(mt)
            }
            Err(e) => {
                // `prep_spawn` froze this id, so nothing else will hand it back.
                tmgr.release_super_tid(super_tid);
                Err(e)
            }
        }
    }

    /// Spawn a thread into a given compartment, using initial thread arguments.
    ///
    /// See [`comp_spawn_entry`] for why the entry is a plain fn rather than a boxed closure.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn spawn_compartment_thread(
        &self,
        instance: ObjID,
        args: ThreadSpawnArgs,
        stack_ptr: usize,
        thread_ptr: usize,
    ) -> Result<ObjID, TwzError> {
        let thread = self.start_thread(
            instance,
            comp_spawn_entry,
            thread::EntryArgs {
                instance,
                stack_ptr,
                stack_size: args.stack_size,
                thread_ptr,
                entry: args.start,
                arg: args.arg,
                suspend: false,
            },
        )?;
        let mon = get_monitor();

        // Map the repr into the caller's compartment now, while we still hold `thread` and the
        // object is therefore guaranteed to exist. The caller maps it immediately after this gate
        // returns, and a short-lived thread can exit and be reaped by the thread cleaner -- which
        // deletes the repr -- inside that window. Doing it here turns the caller's map into a
        // refcount bump on this `Space` entry, which cannot fail, instead of a fresh
        // `sys_object_map` racing the delete.
        //
        // These flags must match the ones the runtime asks for (`impl_spawn`), since `MapInfo` is
        // keyed by flags as well as id. `do_spawn` already mapped the repr under the same key, so
        // this shares that slot rather than consuming another.
        let repr_info = MapInfo {
            id: thread.id,
            flags: MapFlags::READ | MapFlags::WRITE,
        };
        let repr_handle = Space::map(&mon.space, repr_info, instance)
            .inspect_err(|e| tracing::debug!("failed to premap repr of {}: {}", thread.id, e))
            .ok();

        // A read: both of the things done here -- pre-creating the per-thread structure and
        // recording the premapped repr -- now take `&RunComp` and carry their own locks.
        let comps = crate::lockdiag::watched(mon.comp_mgr.read(ThreadKey::get().unwrap()));
        let comp = comps.get(instance)?;
        write_note!(thread.id, "thread:{}", comp.name);
        // Deliberately does *not* touch `get_per_thread`. That creates and maps a simple-buffer
        // object, and doing it here did so for every thread ever spawned, when only threads that
        // make a gate call carrying variable-length data ever read one. `get_per_thread` is an
        // `entry().or_insert_with()` under the compartment's own lock, so leaving it to the first
        // caller is both race-free and one object plus one mapping cheaper per spawn.
        // Held by the compartment, not by us: the caller's `map_object` replaces this entry with
        // its own handle for the same `MapInfo`, and the mapping is released when the caller
        // releases that one.
        if let Some(repr_handle) = repr_handle {
            comp.premap_object(repr_info, repr_handle);
        }
        Ok(thread.id)
    }

    /// Get the compartment config for the given compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_comp_config(&self, sctx: ObjID) -> Result<*const SharedCompConfig, TwzError> {
        let comps = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        Ok(comps.get(sctx)?.comp_config_ptr())
    }

    /// Map an object into a given compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn map_object(&self, sctx: ObjID, info: MapInfo) -> Result<MapHandle, TwzError> {
        let t_space = monmapstats::t0();
        let handle = Space::map(&self.space, info, sctx)?;
        let space_ns = monmapstats::ns(t_space);

        // A read: recording the mapping only touches the compartment's own map, which has its own
        // lock. Taking the manager's *write* lock here made every map in the system serialize
        // against every other one, in any compartment.
        let t_mgr = monmapstats::t0();
        let comp_mgr = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        let rc = comp_mgr.get(sctx)?;
        let mgr_ns = monmapstats::ns(t_mgr);
        let t_rec = monmapstats::t0();
        let handle = rc.map_object(info, handle)?;
        monmapstats::record(space_ns, mgr_ns, monmapstats::ns(t_rec));
        Ok(handle)
    }

    /// Map a pair of objects into a given compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn map_pair(
        &self,
        sctx: ObjID,
        info: MapInfo,
        info2: MapInfo,
    ) -> Result<(MapHandle, MapHandle), TwzError> {
        let (handle, handle2) =
            crate::lockdiag::watched(self.space.lock().unwrap()).map_pair(info, info2)?;

        let comp_mgr = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        let rc = comp_mgr.get(sctx)?;
        let handle = rc.map_object(info, handle)?;
        let handle2 = rc.map_object(info2, handle2)?;
        Ok((handle, handle2))
    }

    /// Unmap an object from a given compartmen.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn unmap_object(&self, sctx: ObjID, info: MapInfo) {
        let Some(key) = ThreadKey::get() else {
            // The caller is monitor code already holding a monitor lock -- it dropped an
            // `ObjectHandle`, and the runtime's release path came back in through the unmap gate.
            // `comp_mgr` is not takeable from here (happylock hands each thread one key), and the
            // old behaviour of returning leaked the mapping outright. Hand it to the unmapper,
            // which runs on a thread holding no key.
            crate::lockdiag::note_recursive_unmap();
            match self.unmapper.get() {
                Some(unmapper) => unmapper.background_unmap_comp(sctx, info),
                // Before the unmapper exists there is no thread to defer to, and this is still
                // better than a silent return.
                None => tracing::warn!("dropped recursive unmap of {:?}: no unmapper", info),
            }
            return;
        };

        let comp_mgr = crate::lockdiag::watched(self.comp_mgr.read(key));
        if let Ok(comp) = comp_mgr.get(sctx) {
            // Handle dropped after both locks: it sends to the unmapper thread.
            let handle = comp.unmap_object(info);
            drop(comp_mgr);
            drop(handle);
        }
    }

    /// Get the object ID for this compartment-thread's simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_thread_simple_buffer(&self, sctx: ObjID, thread: ObjID) -> Result<ObjID, TwzError> {
        ptstats::record(ptstats::Site::GetSb);
        let pt = self.per_thread(sctx, thread)?;
        let id = pt.lock().unwrap().simple_buffer_id();
        id.ok_or(GenericError::Internal.into())
    }

    /// Reach a compartment thread's per-thread data under a *read* of the compartment manager.
    ///
    /// The manager lock is dropped before the caller touches the buffer, so a memcpy into a
    /// thread's own object no longer excludes every other monitor operation. See
    /// [`RunComp::get_per_thread`] for why an owned handle is sound here.
    fn per_thread(
        &self,
        sctx: ObjID,
        thread: ObjID,
    ) -> Result<std::sync::Arc<Mutex<compartment::PerThread>>, TwzError> {
        let comps = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        Ok(comps.get(sctx)?.get_per_thread(thread))
    }

    /// Write bytes to this per-compartment thread's simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn _write_thread_simple_buffer(
        &self,
        sctx: ObjID,
        thread: ObjID,
        bytes: &[u8],
    ) -> Result<usize, TwzError> {
        let pt = self.per_thread(sctx, thread)?;
        let n = pt.lock().unwrap().write_bytes(bytes);
        Ok(n)
    }

    /// Read bytes from this per-compartment thread's simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn read_thread_simple_buffer(
        &self,
        sctx: ObjID,
        thread: ObjID,
        len: usize,
        site: ptstats::Site,
    ) -> Result<Vec<u8>, TwzError> {
        ptstats::record(site);
        let pt = self.per_thread(sctx, thread)?;
        let bytes = pt.lock().unwrap().read_bytes(len);
        Ok(bytes)
    }

    /// Read the name of a compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn comp_name(&self, id: ObjID) -> Result<String, TwzError> {
        self.comp_mgr
            .read(ThreadKey::get().unwrap())
            .get(id)
            .map(|rc| rc.name.clone())
    }

    pub fn upcall_handle(
        &self,
        frame: &mut UpcallFrame,
        info: &UpcallData,
    ) -> Result<Option<ResumeFlags>, TwzError> {
        // An upcall is delivered on whichever thread faulted, and that thread can already be inside
        // monitor code holding the key -- `ThreadKey::get` returns None exactly then, and this
        // unwrapped it. The panic reported itself rather than the fault that caused it, which is
        // how a compartment stack being unmapped under a running thread surfaced as a bare
        // `unwrap` on `None`. The `Err` arm in `upcall_monitor_handler` prints `frame` and
        // `info`, so the real violation reaches the log instead.
        //
        // This does not save the thread: it dies here either way, holding whatever monitor lock it
        // took before faulting. Not faulting is the fix, and it belongs where the unmap happens --
        // see `CompartmentMgr::process_cleanup_queue`.
        self.comp_mgr
            .write(reentrant_key()?)
            .get_mut(frame.prior_ctx)?
            .upcall_handle(frame, info)
    }

    /// Perform a compartment control action on the calling compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn compartment_ctrl(
        &self,
        info: &secgate::GateCallInfo,
        cmd: MonitorCompControlCmd,
    ) -> Option<i32> {
        let src = info.source_context()?;
        tracing::debug!(
            "compartment ctrl from: {:?}, thread = {:?}: {:?}",
            src,
            info.thread_id(),
            cmd
        );
        match cmd {
            // Here, the thread has indicated that it has initialized the runtime (and run
            // constructors), and so is ready to call main. At this point, we make sure
            // no errors have occurred and that we should continue. Update flags to
            // ready via compare-and-swap to ensure no one has set an error flag, and
            // return. If this compartment is a binary, then return None so the runtime will call
            // main. Otherwise return Some(SUCCESS) so that the runtime immediately
            // calls the post-main hook.
            MonitorCompControlCmd::RuntimeReady => loop {
                let state = self.load_compartment_flags(src);
                if state & COMP_STARTED == 0
                    || state & COMP_DESTRUCTED != 0
                    || state & COMP_EXITED != 0
                {
                    tracing::warn!(
                        "runtime main thread {} encountered invalid compartment {} state: {}",
                        info.thread_id(),
                        src,
                        state
                    );
                    sys_thread_exit(127);
                }

                if self.update_compartment_flags(src, |state| Some(state | COMP_READY)) {
                    tracing::debug!(
                        "runtime main thread reached compartment ready state in {}: {:x}",
                        self.comp_name(src)
                            .unwrap_or_else(|_| String::from("unknown")),
                        state
                    );
                    break if state & COMP_IS_BINARY == 0 {
                        Some(0)
                    } else {
                        None
                    };
                }
            },
            MonitorCompControlCmd::RuntimePostMain => {
                // A compartment finishing main is the only moment the monitor is told about that
                // is also a natural end of a measured workload, so the space counters print here
                // rather than on a cadence that would land inside what they measure.
                crate::mon::space::spacesplit::report();
                // First we want to check if we are a binary, and if so, we don't have to wait
                // around in here.
                loop {
                    if self.update_compartment_flags(src, |state| {
                        // Binaries can exit immediately. All future cross-compartment calls fail.
                        if state & COMP_IS_BINARY != 0 {
                            Some(state | COMP_THREAD_CAN_EXIT)
                        } else {
                            Some(state)
                        }
                    }) {
                        tracing::debug!(
                            "runtime main thread reached compartment post-main state in {}",
                            self.comp_name(src)
                                .unwrap_or_else(|_| String::from("unknown"))
                        );
                        break;
                    }
                }
                // Wait until we are allowed to exit (no one has a living, callable reference to us,
                // or we are a binary), ant then set the destructed flag and return.
                loop {
                    let flags = self.load_compartment_flags(src);
                    if flags & COMP_THREAD_CAN_EXIT != 0
                        && self.update_compartment_flags(src, |state| Some(state | COMP_DESTRUCTED))
                    {
                        tracing::debug!(
                            "runtime main thread destructing in {}",
                            self.comp_name(src)
                                .unwrap_or_else(|_| String::from("unknown"))
                        );
                        break None;
                    }
                    self.wait_for_compartment_state_change(src, flags);
                }
            }
            // Process-exit requested from a non-main thread (`exit(2)` semantics: any thread may
            // end the process). Only pin the code and force-exit the threads; teardown proper
            // stays with the cleaner's `main_thread_exited` pass when the main thread's death is
            // reaped, so dep use-counts are decremented exactly once. A thread spawned while this
            // runs is caught by that same pass, which recollects the thread list.
            MonitorCompControlCmd::Exit(code) => {
                let threads = {
                    let Ok(key) = reentrant_key() else {
                        sys_thread_exit(code as u64);
                    };
                    let (ref tmgr, ref cmgr, _, _, _) =
                        *crate::lockdiag::watched(self.locks.lock(key));
                    let Ok(rc) = cmgr.get(src) else {
                        sys_thread_exit(code as u64);
                    };
                    rc.set_exit_code(code);
                    rc.thread_ids_including(&tmgr.threads_of(src))
                };
                // Delivery of each exit is gated on the thread's home context, stamped at spawn:
                // a thread mid-gate elsewhere dies at home, not holding a foreign compartment's
                // locks. The caller is in this set (it is mid-gate here), so its own exit lands
                // once it crosses back; the runtime exits it directly regardless.
                for thread in threads {
                    let _ = sys_thread_change_state(
                        thread,
                        twizzler_abi::thread::ExecutionState::Exited,
                    );
                }
                Some(code)
            }
        }
    }

    pub fn post_signal(
        &self,
        info: &secgate::GateCallInfo,
        target: Option<ObjID>,
        signal: u64,
        flags: PostSignalFlags,
    ) -> Result<(), TwzError> {
        let target = target.unwrap_or(info.source_context().unwrap_or(MONITOR_INSTANCE_ID));
        // Both sinks below are bitmasks indexed by the signal number, so bound it here rather
        // than shifting by whatever the caller passed.
        if signal == 0 || signal >= u64::BITS as u64 {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let post_signal = |target: ObjID, sig: u64| -> Result<(), TwzError> {
            tracing::debug!("posting signal {} to {}", sig, target);
            let comp = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
            let comp = comp.get(target)?;
            let scc = comp.comp_config_ptr();
            let scc = unsafe { &*scc };
            scc.post_signal(signal);
            if let Some(thread) = comp.main_thread() {
                sys_thread_send_message(thread.thread.id, signal, 0)?;
            }
            Ok(())
        };
        if flags.contains(PostSignalFlags::GROUP) {
            return Err(TwzError::NOT_SUPPORTED);
        }
        if flags.contains(PostSignalFlags::CONTROLLER) {
            let targets = self
                .comp_mgr
                .read(ThreadKey::get().unwrap())
                .find_controller_targets(target);
            for t in targets {
                let _ = post_signal(t, signal).inspect_err(|e| {
                    tracing::warn!(
                        "failed to raise signal via controller {} to target {}: {}",
                        target,
                        t,
                        e
                    )
                });
            }
        } else {
            post_signal(target, signal)?;
        }
        return Ok(());
    }

    pub fn set_controller(
        &self,
        _info: &secgate::GateCallInfo,
        target: ObjID,
        controller: ObjID,
    ) -> Result<(), TwzError> {
        let mut cm = crate::lockdiag::watched(self.comp_mgr.write(ThreadKey::get().unwrap()));
        cm.set_controller(target, controller)?;
        return Ok(());
    }

    pub fn libname_map(
        &self,
        caller: ObjID,
        thread: ObjID,
        namelen: usize,
        id: ObjID,
    ) -> Result<(), TwzError> {
        let str_bytes =
            self.read_thread_simple_buffer(caller, thread, namelen, ptstats::Site::LibNameMap)?;

        let name = str::from_utf8(&str_bytes).map_err(|_| TwzError::INVALID_ARGUMENT)?;
        tracing::trace!("libname map: {}", name);
        let mut dynlink = crate::lockdiag::watched(self.dynlink.write(ThreadKey::get().unwrap()));
        dynlink.engine.add_name_map(name, id);

        Ok(())
    }

    pub fn libname_unmap(
        &self,
        caller: ObjID,
        thread: ObjID,
        namelen: Option<usize>,
        id: Option<ObjID>,
    ) -> Result<(), TwzError> {
        let str_bytes = self.read_thread_simple_buffer(
            caller,
            thread,
            namelen.unwrap_or(0),
            ptstats::Site::LibNameUnmap,
        )?;
        let name = namelen
            .map(|_| str::from_utf8(&str_bytes).map_err(|_| TwzError::INVALID_ARGUMENT))
            .transpose()?;
        let mut dynlink = crate::lockdiag::watched(self.dynlink.write(ThreadKey::get().unwrap()));
        dynlink.engine.remove_name_map(name, id);

        Ok(())
    }
}

static MONITOR: OnceLock<Monitor> = OnceLock::new();

/// Get the monitor instance. Panics if called before first call to [set_monitor].
pub fn get_monitor() -> &'static Monitor {
    MONITOR.get().unwrap()
}

/// Set the monitor instance. Can only be called once. Must be called before any call to
/// [get_monitor].
pub fn set_monitor(monitor: Monitor) {
    if MONITOR.set(monitor).is_err() {
        panic!("second call to set_monitor");
    }
}

pub use space::early_object_map;
