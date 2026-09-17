use std::{
    collections::BTreeSet,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use fotcache::FotCache;
use handlecache::HandleCache;
use tracing::warn;
use twizzler_abi::syscall::{
    sys_map_ctrl, sys_object_create, sys_object_ctrl, sys_object_preload_range,
    sys_object_read_map, CreateTieFlags, CreateTieSpec, DeleteFlags, MapControlCmd,
    ObjectControlCmd, ObjectCreate, ObjectCreateFlags, PreloadRangeSpec,
};
use twizzler_rt_abi::{
    bindings::{
        object_cmd, object_handle, object_source, object_tie, release_flags, RELEASE_NO_CACHE,
    },
    error::{ObjectError, ResourceError, TwzError},
    object::{
        FotEntry, FotFlags, MapFlags, ObjID, ObjectCmd, ObjectHandle, MAX_SIZE, MEXT_SIZED,
        NULLPAGE_SIZE,
    },
    Result,
};

use super::ReferenceRuntime;
use crate::runtime::file::get_naming_handle;

mod fotcache;
mod handlecache;

#[repr(C)]
pub(crate) struct RuntimeHandleInfo {
    refs: AtomicU64,
    fot_cache: FotCache,
    is_deleted: AtomicBool,
}

pub(crate) fn new_runtime_info() -> *mut RuntimeHandleInfo {
    let rhi = Box::new(RuntimeHandleInfo {
        refs: AtomicU64::new(1),
        fot_cache: FotCache::new(),
        is_deleted: AtomicBool::new(false),
    });
    Box::into_raw(rhi)
}

pub(crate) fn free_runtime_info(ptr: *mut RuntimeHandleInfo) {
    if ptr.is_null() {
        return;
    }
    let _boxed = unsafe { Box::from_raw(ptr) };
}

pub(crate) fn new_object_handle(id: ObjID, slot: usize, flags: MapFlags) -> ObjectHandle {
    unsafe {
        ObjectHandle::new(
            id,
            new_runtime_info().cast(),
            (slot * MAX_SIZE) as *mut _,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut _,
            flags,
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }
}

/// Upper bound (exclusive) on FOT indices for `handle`, derived from the actual extent of the
/// FOT region (`meta - start`, per `new_object_handle`) rather than trusting a caller-supplied
/// index -- `idx` can originate from data resolved out of an object's own (possibly untrusted)
/// memory, so an unbounded `idx` must not turn into out-of-bounds pointer arithmetic.
fn max_fot_idx(handle: &object_handle) -> u64 {
    ((handle.meta as usize) - (handle.start as usize)) as u64
        / std::mem::size_of::<FotEntry>() as u64
}

/// Bumped every time a key leaves the in-flight set. Waiters sleep on it rather than spinning
/// through another thread's gate call, which can be a millisecond when the object is cold.
static INFLIGHT_GEN: AtomicU64 = AtomicU64::new(0);

fn inflight_wake() {
    INFLIGHT_GEN.fetch_add(1, Ordering::SeqCst);
    let _ = twizzler_abi::syscall::sys_thread_sync(
        &mut [twizzler_abi::syscall::ThreadSync::new_wake(
            twizzler_abi::syscall::ThreadSyncWake::new(
                twizzler_abi::syscall::ThreadSyncReference::Virtual(&INFLIGHT_GEN),
                usize::MAX,
            ),
        )],
        None,
    );
}

/// Sleep until [`INFLIGHT_GEN`] moves off `gen`. Read `gen` *before* the check that decided to
/// wait: `ThreadSyncSleep` refuses to sleep when the word already differs, so a waker that lands in
/// between cannot be lost.
fn inflight_wait(gen: u64) {
    let _ = twizzler_abi::syscall::sys_thread_sync(
        &mut [twizzler_abi::syscall::ThreadSync::new_sleep(
            twizzler_abi::syscall::ThreadSyncSleep::new(
                twizzler_abi::syscall::ThreadSyncReference::Virtual(&INFLIGHT_GEN),
                gen,
                twizzler_abi::syscall::ThreadSyncOp::Equal,
                twizzler_abi::syscall::ThreadSyncFlags::empty(),
            ),
        )],
        None,
    );
}

impl ReferenceRuntime {
    /// Issue the monitor unmap gate for each key, then release the keys and wake any waiters.
    ///
    /// Called with the manager mutex dropped and every key already marked in-flight, so a
    /// concurrent map of the same object waits for the unmap instead of racing it.
    fn finish_unmaps(&self, keys: Vec<ObjectMapKey>) {
        if keys.is_empty() {
            return;
        }
        for key in &keys {
            let _ = monitor_api::monitor_rt_object_unmap(key.0, key.1)
                .inspect_err(|e| warn!("failed to unmap {:?}: {}", key, e));
        }
        let mut mgr = self.object_manager.lock();
        for key in &keys {
            mgr.end_inflight(*key);
        }
        drop(mgr);
        inflight_wake();
    }

    #[tracing::instrument(ret, skip(self), level = "trace")]
    pub fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle> {
        let key = ObjectMapKey(id.into(), flags);
        loop {
            // Sampled before the lock: if the owner of this key finishes while we are queued for
            // the mutex, the generation has already moved and the sleep below returns at once.
            let gen = INFLIGHT_GEN.load(Ordering::SeqCst);
            let mut mgr = self.object_manager.lock();
            if let Some(handle) = mgr.cached(key) {
                let pending = mgr.begin_unmaps();
                drop(mgr);
                self.finish_unmaps(pending);
                return Ok(handle);
            }
            if !mgr.begin_inflight(key) {
                // Another thread is inside the gate call for this exact key. Its result will land
                // in the cache, so wait for it rather than issuing a second map -- the monitor
                // tracks a compartment's mappings by MapInfo alone, with no reference count.
                drop(mgr);
                inflight_wait(gen);
                continue;
            }
            break;
        }

        // Mutex dropped: this is the whole point. The gate call reaches into the monitor and can
        // block in the kernel on a pager round trip for a cold object, and holding a
        // per-compartment mutex across that serializes every unrelated map in the process
        // behind it.
        let mapping = monitor_api::monitor_rt_object_map(key.0, key.1);

        let mut mgr = self.object_manager.lock();
        mgr.end_inflight(key);
        let res = mapping.map(|mapping| mgr.insert_mapped(key, mapping.slot));
        let pending = mgr.begin_unmaps();
        drop(mgr);
        inflight_wake();
        self.finish_unmaps(pending);
        res
    }

    pub fn create_rtobj(&self) -> Result<ObjID> {
        let tie_id = monitor_api::get_comp_config().sctx;
        let mut create = ObjectCreate::default();
        create.flags = ObjectCreateFlags::DELETE;
        sys_object_create(
            create,
            &[],
            &[CreateTieSpec::new(tie_id, CreateTieFlags::empty()).into()],
        )
    }

    pub fn update_handle(&self, handle: *mut object_handle) -> Result<()> {
        sys_map_ctrl(
            unsafe { &*handle }.start.cast(),
            MAX_SIZE,
            MapControlCmd::Update,
            0,
        )?;
        unsafe { &*(&*handle).runtime_info.cast::<RuntimeHandleInfo>() }
            .fot_cache
            .clear();
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "trace")]
    pub fn release_handle(&self, handle: *mut object_handle, flags: release_flags) {
        let mut mgr = self.object_manager.lock();
        mgr.release(handle, flags);
        // TODO: do this less often?
        if self.is_monitor().is_some() {
            mgr.cache.flush();
        }
        let pending = mgr.begin_unmaps();
        drop(mgr);
        self.finish_unmaps(pending);
    }

    /// Unmap cached handles that have gone stale. Called from `twz_rt_gc`.
    pub fn gc_object_cache(&self) {
        let mut mgr = self.object_manager.lock();
        mgr.cache.sweep();
        let pending = mgr.begin_unmaps();
        drop(mgr);
        self.finish_unmaps(pending);
    }

    pub fn object_cmd(
        &self,
        handle: *mut object_handle,
        cmd: object_cmd,
        arg: *mut c_void,
    ) -> Result<()> {
        let cmd: ObjectCmd = cmd.try_into()?;
        let handle = unsafe { &*handle };
        match cmd {
            ObjectCmd::Delete => {
                sys_object_ctrl(
                    handle.id.into(),
                    ObjectControlCmd::Delete(DeleteFlags::empty()),
                    0,
                    0,
                )?;
                unsafe { &*handle.runtime_info.cast::<RuntimeHandleInfo>() }
                    .is_deleted
                    .store(true, Ordering::Release);
                Ok(())
            }
            ObjectCmd::Sync => sys_map_ctrl(
                handle.start.cast(),
                MAX_SIZE,
                MapControlCmd::Sync(arg.cast()),
                0,
            ),
            ObjectCmd::Update => {
                sys_map_ctrl(handle.start.cast(), MAX_SIZE, MapControlCmd::Update, 0)
            }
            // Whole-object preload, sized to the object rather than to `MAX_SIZE`.
            //
            // `ObjectControlCmd::Preload` submits one range of `MAX_SIZE / PAGE_SIZE` pages
            // whatever the object's real size is -- 1% of over-ask on a segment filling its
            // object, 66x on a 15 MiB one. The length is right here on the handle, so ask for
            // what exists. Without `MEXT_SIZED` no length is knowable and the whole extent is
            // the only honest request.
            ObjectCmd::Preload => {
                let oh = ObjectHandle::from_raw(*handle);
                let len = oh
                    .find_meta_ext(MEXT_SIZED)
                    .map(|me| me.value.load(Ordering::SeqCst));
                std::mem::forget(oh);
                match len {
                    Some(len) if len > 0 => sys_object_preload_range(
                        handle.id.into(),
                        &[PreloadRangeSpec::from_bytes(0, len)],
                    ),
                    _ => sys_object_ctrl(handle.id.into(), ObjectControlCmd::Preload, 0, 0)
                        .map(|_| ()),
                }
            }
        }
    }

    pub fn create_object(
        &self,
        spec: &ObjectCreate,
        src: &[object_source],
        ties: &[object_tie],
        name: Option<&str>,
    ) -> Result<ObjID> {
        let id = sys_object_create(*spec, src, ties)?;
        if let Some(name) = name {
            if let Some(nh) = get_naming_handle() {
                nh.put(name, id)?;
            } else {
                tracing::warn!("tried to bind object name {} before naming is setup", name);
            }
        }
        Ok(id)
    }

    pub fn get_object_handle_from_ptr(&self, ptr: *const u8) -> Result<object_handle> {
        let mut mgr = self.object_manager.lock();
        let cached = mgr.get_handle(ptr);
        // `activate` can retire a mapping (a failed reactivate), and the cache no longer unmaps
        // for itself. Draining costs nothing when empty.
        let pending = mgr.begin_unmaps();
        drop(mgr);
        self.finish_unmaps(pending);
        if let Some(handle) = cached {
            return Ok(handle);
        }

        let slot = ptr as usize / MAX_SIZE;
        let Some(id) = self.get_id_from_heap_ptr(ptr) else {
            let map = sys_object_read_map(None, slot)?;
            return Ok(object_handle {
                id: map.id.raw(),
                start: (slot * MAX_SIZE) as *mut c_void,
                map_flags: map.flags.bits(),
                ..Default::default()
            });
        };
        Ok(object_handle {
            id: id.raw(),
            start: (slot * MAX_SIZE) as *mut c_void,
            map_flags: (MapFlags::READ | MapFlags::WRITE).bits(),
            ..Default::default()
        })
    }

    pub fn insert_fot(&self, handle: *mut object_handle, fot: *const u8) -> Result<u32> {
        //tracing::warn!("TODO: insert FOT entry");
        let handle = unsafe { &*handle };
        let _meta = unsafe { &*handle.meta };
        let new_fot = unsafe { fot.cast::<FotEntry>().read() };
        for i in 1..max_fot_idx(handle) as u32 {
            let ptr = unsafe { &mut *handle.meta.cast::<FotEntry>().sub((i + 1) as usize) };
            let flags = FotFlags::from_bits_truncate(ptr.flags.load(Ordering::SeqCst));

            if flags.contains(FotFlags::ALLOCATED)
                && flags.contains(FotFlags::ACTIVE)
                && !flags.contains(FotFlags::DELETED)
            {
                if ptr.values == new_fot.values && ptr.resolver == new_fot.resolver {
                    return Ok(i);
                }
            }

            if flags.contains(FotFlags::DELETED)
                || (!flags.contains(FotFlags::ACTIVE) && !flags.contains(FotFlags::ALLOCATED))
            {
                if let Ok(_) = ptr.flags.compare_exchange(
                    flags.bits(),
                    FotFlags::ALLOCATED.bits(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    let mut flags =
                        FotFlags::from_bits_truncate(new_fot.flags.load(Ordering::SeqCst));
                    flags.set(FotFlags::DELETED, false);
                    flags.set(FotFlags::ALLOCATED, true);
                    flags.set(FotFlags::ACTIVE, true);
                    ptr.values = new_fot.values;
                    ptr.resolver = new_fot.resolver;
                    ptr.flags.store(flags.bits(), Ordering::SeqCst);
                    return Ok(i);
                }
            }
        }
        Err(ResourceError::OutOfResources.into())
    }

    fn read_fot_entry(&self, handle: &object_handle, idx: u64) -> Result<FotEntry> {
        if idx == 0 || idx >= max_fot_idx(handle) {
            return Err(ObjectError::InvalidFote.into());
        }
        let ptr = unsafe { &*handle.meta.cast::<FotEntry>().sub((idx + 1) as usize) };
        let flags = FotFlags::from_bits_truncate(ptr.flags.load(Ordering::SeqCst));
        if flags.contains(FotFlags::DELETED)
            || !flags.contains(FotFlags::ACTIVE)
            || !flags.contains(FotFlags::ALLOCATED)
        {
            return Err(ObjectError::InvalidFote.into());
        }
        if flags.contains(FotFlags::RESOLVER) {
            return Err(TwzError::NOT_SUPPORTED);
        }
        let val = unsafe { (ptr as *const FotEntry).read_volatile() };

        let flags = FotFlags::from_bits_truncate(val.flags.load(Ordering::SeqCst));
        if flags.contains(FotFlags::DELETED)
            || !flags.contains(FotFlags::ACTIVE)
            || !flags.contains(FotFlags::ALLOCATED)
        {
            return Err(ObjectError::InvalidFote.into());
        }
        Ok(val)
    }

    pub fn resolve_fot(
        &self,
        handle: *mut object_handle,
        idx: u64,
        _valid_len: usize,
        map_flags: MapFlags,
    ) -> Result<ObjectHandle> {
        if idx == 0 || handle.is_null() {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let handle = unsafe { &*handle };
        tracing::trace!("Resolving FOT: {:x}", handle.id);
        let entry = self.read_fot_entry(handle, idx)?;
        let id = ObjID::from_parts(entry.values);

        let res_handle = self.map_object(id, map_flags)?;
        unsafe { &*handle.runtime_info.cast::<RuntimeHandleInfo>() }
            .fot_cache
            .insert(idx, map_flags, res_handle.clone());
        Ok(res_handle)
    }

    pub fn resolve_fot_local(
        &self,
        ptr: *mut u8,
        idx: u64,
        _valid_len: usize,
        flags: MapFlags,
    ) -> *mut u8 {
        let mut mgr = self.object_manager.lock();
        let cached = mgr.get_handle(ptr);
        let pending = mgr.begin_unmaps();
        drop(mgr);
        self.finish_unmaps(pending);
        if let Some(handle) = cached {
            tracing::trace!("Resolving FOT local: {:x}", handle.id);
            let rtinfo: *const RuntimeHandleInfo = handle.runtime_info.cast();
            unsafe {
                return (&*rtinfo)
                    .fot_cache
                    .resolve_cached_ptr(idx, flags)
                    .unwrap_or(core::ptr::null_mut());
            }
        }
        core::ptr::null_mut()
    }

    pub fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle)> {
        let mapping =
            monitor_api::monitor_rt_object_pair_map(in_id_a, in_flags_a, in_id_b, in_flags_b)?;

        let handle = new_object_handle(in_id_a, mapping.0.slot, in_flags_a);
        let handle2 = new_object_handle(in_id_b, mapping.1.slot, in_flags_b);
        Ok((handle, handle2))
    }
}

/// A key for local (per-compartment) mappings of objects.
#[derive(PartialEq, PartialOrd, Ord, Eq, Hash, Copy, Clone, Debug)]
pub struct ObjectMapKey(pub ObjID, pub MapFlags);

impl ObjectMapKey {
    pub fn from_raw_handle(handle: &object_handle) -> Self {
        Self(
            handle.id.into(),
            MapFlags::from_bits_truncate(handle.map_flags),
        )
    }
}

/// Per-compartment object management.
pub struct ObjectHandleManager {
    cache: HandleCache,
    /// Keys with a monitor map or unmap gate call in flight, held while this mutex is dropped.
    ///
    /// The monitor records a compartment's mappings in a plain `MapInfo`-keyed map with no
    /// reference count, so two gate calls for one key overlapping can leave the compartment with
    /// no record of a mapping it still uses -- an unmap that lands after a map removes the
    /// entry the map just installed. Serializing per key preserves exactly the ordering the
    /// single mutex used to give; what it drops is the serialization between *different* keys,
    /// which shared nothing but this mutex.
    inflight: BTreeSet<ObjectMapKey>,
}

impl ObjectHandleManager {
    pub const fn new() -> Self {
        Self {
            cache: HandleCache::new(),
            inflight: BTreeSet::new(),
        }
    }

    /// Take ownership of the gate call for `key`. False if another thread already owns it.
    pub fn begin_inflight(&mut self, key: ObjectMapKey) -> bool {
        self.inflight.insert(key)
    }

    pub fn end_inflight(&mut self, key: ObjectMapKey) {
        self.inflight.remove(&key);
    }

    /// Claim the unmaps the cache has queued, marking each in-flight so the caller can issue their
    /// gate calls with this mutex dropped.
    /// A key already in-flight is *not* claimed: it is put back for a later drain.
    ///
    /// `insert` returning false means a `map_object` for that same key already owns the in-flight
    /// marker and is inside its gate call. Claiming it anyway did two damaging things: the unmap
    /// gate call went out concurrently with that map, and the `end_inflight` in `finish_unmaps`
    /// then cleared the *mapper's* marker, so a third thread could start a second concurrent map
    /// of a key the monitor tracks by MapInfo alone. The first is the fault caught in
    /// `many-extcount/round11`: slot 0x125 recorded `mapped -> comp-unmap requested -> count->0 ->
    /// unmapped`, and the thread still inside `RawFile::open` for that object read its meta page
    /// (`extcount`, meta+0x26) after the slot had gone.
    pub fn begin_unmaps(&mut self) -> Vec<ObjectMapKey> {
        let pending = self.cache.take_pending_unmaps();
        let mut claimed = Vec::with_capacity(pending.len());
        let mut deferred = Vec::new();
        for key in pending {
            if self.inflight.insert(key) {
                claimed.push(key);
            } else {
                deferred.push(key);
            }
        }
        self.cache.requeue_unmaps(deferred);
        claimed
    }

    /// The handle for `key` if it is already mapped by this compartment.
    pub fn cached(&mut self, key: ObjectMapKey) -> Option<ObjectHandle> {
        let handle = self.cache.activate(key)?;
        let oh = ObjectHandle::from_raw(handle);
        let oh2 = oh.clone();
        std::mem::forget(oh);
        Some(oh2)
    }

    /// Record a mapping this compartment just obtained from the monitor.
    pub fn insert_mapped(&mut self, key: ObjectMapKey, slot: usize) -> ObjectHandle {
        // This key is mapped again, so any unmap still queued for it is stale -- see
        // `HandleCache::cancel_pending_unmap`. Deferring rather than dropping it would hand the
        // next drain an unmap of the mapping being established here.
        self.cache.cancel_pending_unmap(&key);
        let handle = new_object_handle(key.0, slot, key.1).into_raw();
        self.cache.insert(handle);
        ObjectHandle::from_raw(handle)
    }

    /// Get an object handle from a pointer to within that object.
    pub fn get_handle(&mut self, ptr: *const u8) -> Option<object_handle> {
        let handle = self.cache.activate_from_ptr(ptr)?;
        let oh = ObjectHandle::from_raw(handle);
        let oh2 = oh.clone().into_raw();
        std::mem::forget(oh);
        Some(oh2)
    }

    /// Release a handle. If all handles have been released, calls to monitor to unmap.
    pub fn release(&mut self, handle: *mut object_handle, mut flags: release_flags) {
        let handle = unsafe { handle.as_mut().unwrap() };
        let rhi = unsafe { &*handle.runtime_info.cast::<RuntimeHandleInfo>() };
        // Resurrect check, and the whole point of doing it here.
        //
        // `ObjectHandle::drop` decides to release *outside* this lock: it decrements, sees the old
        // value was 1, and only then calls in. `cached` hands out a fresh reference to a still-
        // `active` entry from *inside* this lock. So between the decrement and this call another
        // thread can legitimately be holding this handle, and releasing anyway unmaps the slot out
        // from under it -- which is the extcount fault (`many-extcount/round11`,
        // `many-extfix2/round3`): a thread inside `RawFile::open` read meta+0x26 of a slot whose
        // mapping had just gone.
        //
        // Checking under the lock is what orders the two: either the increment got here first and
        // we abandon the release, or we got here first and the entry leaves `active`, so no later
        // `cached` can find it. That ordering is also what makes `clone`'s `Relaxed` increment
        // sound -- the mutex supplies the edge.
        if rhi.refs.load(Ordering::Acquire) != 0 {
            return;
        }
        if rhi.is_deleted.load(Ordering::Acquire) {
            flags |= RELEASE_NO_CACHE;
        }
        self.cache.release(handle, flags);
    }
}
