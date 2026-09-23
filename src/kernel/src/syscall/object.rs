use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use twizzler_abi::{
    meta::{MetaFlags, MetaInfo},
    object::{MAX_SIZE, ObjID, Protections},
    pager::PagerFlags,
    syscall::{
        EnumerateKind, HandleType, MAX_PRELOAD_RANGES, MapControlCmd, MapFlags, MapInfo,
        ObjectControlCmd, ObjectCreate, ObjectCreateFlags, ObjectInfo, PreloadRangeSpec,
    },
};
use twizzler_rt_abi::{
    Result,
    bindings::{object_source, object_tie},
    error::{ArgumentError, NamingError, ObjectError, ResourceError, TwzError},
    object::Nonce,
};

use crate::{
    arch::context::ArchContext,
    memory::context::{Context, ContextRef, UserContext, virtmem::Slot},
    mutex::Mutex,
    obj::{LookupFlags, Object, ObjectRef, PageNumber, id::calculate_new_id, lookup_object},
    once::OnceWait,
    random::getrandom,
    security::{KERNEL_SCTX, get_sctx},
    syscall::create_user_slice,
    thread::{current_memory_context, current_thread_ref},
};

/// The calling thread's memory context, as an error rather than a panic -- these are all reachable
/// straight from userspace.
///
/// It logs because the two ways it can fail want different fixes and the count alone cannot tell
/// them apart. `Some(id)` means the thread exists but was built with no context, which is permanent
/// (the field is written once, in `Thread::new`) and belongs to whoever spawned it. `None` means
/// there was no current thread at all during a syscall, which should be impossible -- though
/// Post-boot windows with no current thread do occur.
fn current_vmc() -> Result<ContextRef> {
    current_memory_context().ok_or_else(|| {
        // `objid` and the idle flag are what distinguish the two remaining explanations. Since
        // `start_new_user` refuses to build a context-less user thread, a thread reported here is
        // expected to be a kernel or idle thread -- which would mean the syscall's caller is *not*
        // the thread named as current, rather than that the caller lacks a context.
        let cur = current_thread_ref();
        log::warn!(
            "syscall needing a memory context ran without one (current thread: {:?}, objid: {:?}, \
             idle: {:?}, flags: {:?})",
            cur.map(|t| t.id()),
            cur.map(|t| t.objid()),
            cur.map(|t| t.is_idle_thread()),
            cur.map(|t| t.flags.load(core::sync::atomic::Ordering::SeqCst)),
        );
        TwzError::NOT_SUPPORTED
    })
}

/// Who is mapping, for `sys_object_map`'s failure warnings.
///
/// A failed map is diagnosed from the caller, not the object: the id alone does not say which
/// thread or security context lost the race against a delete. Written as a `Display` rather than a
/// helper returning a string so the warning path allocates nothing.
struct MapCaller;

impl core::fmt::Display for MapCaller {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match current_thread_ref() {
            Some(ct) => write!(
                f,
                "thread {} ({}), sctx {}",
                ct.id(),
                ct.objid(),
                ct.active_sctx_id()
            ),
            None => write!(f, "no current thread"),
        }
    }
}

fn new_nonce() -> Result<u128> {
    let mut bytes = [0; 16];
    if !getrandom(&mut bytes, false) {
        let e = TwzError::Resource(ResourceError::OutOfResources);
        Err(e)
    } else {
        Ok(u128::from_ne_bytes(bytes))
    }
}

pub fn sys_object_create(
    create: &ObjectCreate,
    srcs: &[object_source],
    ties: &[object_tie],
) -> Result<ObjID> {
    let nonce = if create.flags.contains(ObjectCreateFlags::NO_NONCE) {
        0
    } else {
        new_nonce()?
    };
    let id = calculate_new_id(create.kuid, MetaFlags::default(), nonce, create.def_prot);
    let inner = Object::new(id, create.lt, ties);
    let obj = Arc::new(inner);
    // Nothing is on the store until the first sync, whatever sources were copied in here -- those
    // land in pages, not on disk. Recording zero rather than leaving it unknown is what lets the
    // fault path fill a brand-new object without a single pager round trip.
    obj.set_known_len(0);
    // Exact: nothing is on the store until the first sync, so the length is precisely 0 -- not the
    // page-granular extent a stored object reports. This is what lets a fault past EOF be
    // zero-filled rather than paged in for a brand-new object (see `ensure_in_core_pager`).
    obj.mark_known_len_exact();
    // We just derived the ID from these fields, so the check `check_id` would do is already done.
    // Recording it now saves the first mapper of this object a `read_meta`, which for a
    // pager-backed object is a page-in.
    obj.set_verified_id(true, create.def_prot);
    // Bound every source range before applying any of them, so a bad entry rejects the whole call
    // rather than leaving the earlier ones written into a half-built object.
    //
    // `zero_range`/`copy_range` pass the offset through to `setup_zero_range`, which does
    // `VirtAddr::new(offset).unwrap()` (obj/pagetables.rs) -- so a `dest_start` landing in the
    // non-canonical hole is a kernel panic reachable from any compartment with a single syscall,
    // and a merely-too-large one silently builds page-table entries outside the object's range,
    // which nothing reports at all. `checked_add` is load-bearing rather than tidy: a plain
    // `start + len <= MAX_SIZE` wraps for a large `len` and *passes*, which is worse than no check
    // because it reads as validated.
    //
    // Bounded at `MAX_SIZE` rather than at the meta page: unlike a copy into a *live* object,
    // where writing `MetaInfo` is never legitimate, a source here targets an object the kernel is
    // still building and whose meta page it writes itself immediately below -- so the meta overlap
    // stays a fall-back-to-`write_meta` case (see `src_wrote_meta`) rather than a rejection.
    let in_object = |start: u64, len: u64| {
        start
            .checked_add(len)
            .is_some_and(|end| end <= MAX_SIZE as u64)
    };
    for src in srcs {
        if !in_object(src.dest_start, src.len) || !in_object(src.src_start, src.len) {
            log::warn!(
                "sys_object_create: source range out of bounds: src_start {:x} dest_start {:x} \
                 len {:x}",
                src.src_start,
                src.dest_start,
                src.len,
            );
            return Err(ArgumentError::InvalidArgument.into());
        }
    }
    if obj.use_pager() {
        // This id is about to exist, so a negative-cache entry for it is stale by construction.
        // Reachable for deterministic ids -- `ino_to_objid` derives one from an inode number, so
        // probing an external file before it is created would otherwise poison it for the boot.
        crate::obj::clear_no_exist(id);
        crate::pager::create_object(id, create, nonce)?;
        if create.flags.contains(ObjectCreateFlags::DELETE) {
            obj.set_delete_on_last_unmap();
        }
        return Ok(obj.id());
    }
    for src in srcs {
        if src.id == 0 {
            obj.zero_range(src.dest_start as usize, src.len as usize)
                .inspect_err(|e| log::error!("failed to zero new object: {}", e))?;
        } else {
            let so = crate::obj::lookup_object(src.id.into(), LookupFlags::empty())
                .ok_or(ObjectError::NoSuchObject)?;
            so.copy_range(
                &obj,
                src.src_start as usize,
                src.dest_start as usize,
                src.len as usize,
            )
            .inspect_err(|e| log::error!("failed to copy range from object {}: {}", src.id, e))?;
        }
    }
    // Whether anything above put a frame under the meta page, which is the one thing that stops
    // this object qualifying for `init_meta` -- see below.
    let meta_offset = PageNumber::meta_page().as_byte_offset();
    let src_wrote_meta = srcs.iter().any(|src| {
        let start = src.dest_start as usize;
        let end = start.saturating_add(src.len as usize);
        start < meta_offset + PageNumber::PAGE_SIZE && end > meta_offset
    });
    let meta = MetaInfo {
        nonce: Nonce(nonce),
        kuid: create.kuid,
        default_prot: create.def_prot,
        flags: MetaFlags::empty(),
        fotcount: 0,
        extcount: 0,
    };
    // `write_meta` goes through the generic fill path -- `ensure_in_core` plus a fresh
    // `FrameAllocator` -- which exists for a page that may already be present, may be COW, may
    // belong to a pager-backed object, and may be raced for by a fault on another cpu. None of
    // that can apply to an object this thread built moments ago, has not registered, and whose
    // pager branch returned above: `init_meta` allocates the frame and installs it directly.
    // The exception is a source that wrote into the meta page, where a frame is already there
    // and must not be replaced.
    if src_wrote_meta {
        while !obj.write_meta(meta) {
            log::error!("failed to write object metadata -- retrying");
        }
    } else {
        obj.init_meta(meta);
    }
    log::trace!(
        "sys_object_create: create={:?}, srcs={}, ties={}: {:?}",
        create,
        srcs.len(),
        ties.len(),
        obj.id(),
    );
    crate::obj::register_object(obj.clone());
    // Deliberately not an immediate delete: `object_ctrl`'s Delete marks the object *and* runs
    // scan_deleted() inline, and a brand-new object has no mappings and no pins, so it was
    // reap-eligible before its creator could map it. The flag means "delete on last unmap", so
    // record that and let the unmap path decide.
    if create.flags.contains(ObjectCreateFlags::DELETE) {
        obj.set_delete_on_last_unmap();
    }
    Ok(obj.id())
}

pub fn sys_object_map(
    id: ObjID,
    slot: usize,
    prot: Protections,
    handle: Option<ObjID>,
    flags: MapFlags,
    target_sctx: ObjID,
) -> Result<usize> {
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(handle).ok_or(ObjectError::NoSuchObject)?
    } else {
        current_vmc()?
    };
    let obj = crate::obj::lookup_object(id, LookupFlags::empty());
    let obj = match obj {
        crate::obj::LookupResult::WasDeleted => {
            log::warn!(
                "sys_object_map: object {} was deleted ({}) [{}]",
                id,
                crate::obj::describe_missing(id),
                MapCaller
            );
            return Err(ObjectError::NoSuchObject.into());
        }
        crate::obj::LookupResult::Found(obj) => obj,
        _ => match crate::pager::lookup_object_and_wait(id) {
            Some(obj) => obj,
            None => {
                log::warn!(
                    "sys_object_map: object {} not found ({}) [{}]",
                    id,
                    crate::obj::describe_missing(id),
                    MapCaller
                );
                return Err(ObjectError::NoSuchObject.into());
            }
        },
    };
    // TODO
    let res =
        crate::operations::map_object_into_context(slot, obj, vm, prot.into(), flags, target_sctx);
    if let Err(e) = res {
        log::warn!(
            "sys_object_map slot {} obj {} sctx {} failed: {:?} [{}]",
            slot,
            id,
            target_sctx,
            e,
            MapCaller
        );
    }
    Ok(slot)
}

pub fn sys_object_unmap(handle: Option<ObjID>, slot: usize) -> Result<u64> {
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(handle).ok_or(ArgumentError::BadHandle)?
    } else {
        current_vmc()?
    };
    vm.remove_object(Slot::try_from(slot).map_err(|_| ArgumentError::InvalidArgument)?);
    Ok(0)
}

pub fn sys_object_readmap(handle: ObjID, slot: usize) -> Result<MapInfo> {
    let vm = if handle.raw() == 0 {
        current_vmc()?
    } else {
        get_vmcontext_from_handle(handle).ok_or(ArgumentError::InvalidArgument)?
    };
    let info = vm.lookup_slot(slot).ok_or(ArgumentError::InvalidAddress)?;
    Ok(MapInfo {
        id: info.object().id(),
        prot: info.mapping_settings(false, false).perms(),
        slot,
        flags: info.flags,
    })
}

pub fn sys_object_info(handle: ObjID) -> Result<ObjectInfo> {
    let obj =
        crate::obj::lookup_object(handle, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
    Ok(obj.info())
}

pub trait ObjectHandle {
    type HandleType;
    fn create_with_handle<NewFn>(obj: ObjectRef, new: NewFn) -> Arc<Self::HandleType>
    where
        NewFn: FnOnce(ObjectRef) -> Arc<Self::HandleType>,
        Self: Sized,
    {
        new(obj)
    }
}

struct Handle<T: ObjectHandle> {
    obj: ObjectRef,
    item: Arc<T::HandleType>,
}

impl<T: ObjectHandle + Clone> Handle<T> {
    fn new<NewFn>(id: ObjID, new: NewFn) -> Result<Self>
    where
        NewFn: FnOnce(ObjectRef) -> Arc<T::HandleType>,
    {
        let obj = crate::obj::lookup_object(id, LookupFlags::empty());
        let obj = match obj {
            crate::obj::LookupResult::Found(obj) => obj,
            _ => return Err(ObjectError::NoSuchObject.into()),
        };
        Ok(Handle {
            obj: obj.clone(),
            item: T::create_with_handle(obj, new),
        })
    }
}

struct AllHandles {
    all: BTreeSet<ObjID>,
    pager_q_count: u8,
    vm_contexts: BTreeMap<ObjID, Handle<ContextRef>>,
}

static ALL_HANDLES: OnceWait<Mutex<AllHandles>> = OnceWait::new();

fn get_all_handles() -> &'static Mutex<AllHandles> {
    ALL_HANDLES.call_once(|| {
        Mutex::new(AllHandles {
            all: BTreeSet::new(),
            vm_contexts: BTreeMap::new(),
            pager_q_count: 0,
        })
    })
}

pub fn count_handles() -> usize {
    get_all_handles().lock().all.len()
}

pub fn get_vmcontext_from_handle(id: ObjID) -> Option<ContextRef> {
    let ah = get_all_handles();
    ah.lock().vm_contexts.get(&id).map(|x| x.item.clone())
}

pub fn sys_new_handle(id: ObjID, handle_type: HandleType) -> Result<u64> {
    let mut ah = get_all_handles().lock();
    if ah.all.contains(&id) {
        return Err(NamingError::AlreadyBound.into());
    }
    match handle_type {
        HandleType::VmContext => ah
            .vm_contexts
            .insert(id, Handle::new(id, |_obj| Context::new())?),
        HandleType::PagerQueue => {
            if ah.pager_q_count == 2 {
                return Err(ResourceError::OutOfNames.into());
            }
            ah.pager_q_count += 1;
            crate::pager::init_pager_queue(id, ah.pager_q_count == 1);
            ah.all.insert(id);
            return Ok(0);
        }
    };
    ah.all.insert(id);
    Ok(0)
}

pub fn sys_unbind_handle(id: ObjID) {
    let mut ah = get_all_handles().lock();
    if !ah.all.contains(&id) {
        return;
    }
    // TODO: we'll need to fix this for having many kinds of handles.
    ah.all.remove(&id);
    ah.vm_contexts.remove(&id).unwrap();
}

// Note: placeholder types
pub fn sys_sctx_attach(id: ObjID) -> Result<u32> {
    // `get_sctx` resolves KERNEL_SCTX now, so say no explicitly: attaching it would let a thread
    // `switch_context(0)` into the context the fault path grants Protections::all() to. This used
    // to be rejected only as a side effect of `lookup_object(ObjID(0))` failing.
    if id == KERNEL_SCTX {
        return Err(ArgumentError::InvalidArgument.into());
    }
    let sctx = get_sctx(id)?;

    let current_thread = current_thread_ref().unwrap();
    let current_context = current_vmc()?;
    // Only build a page-table root if this context doesn't already have one for this sctx.
    // Constructing one unconditionally cost ~590us per call: a global TLB shootdown on the way in,
    // and a walk of the whole user address space in Drop on the way back out. Every gate entry
    // calls this, and after the first it always already exists.
    if current_context.try_with_arch(sctx.id(), |_| ()).is_none() {
        current_context.register_sctx(sctx.id(), ArchContext::new());
    }
    current_thread.secctx.attach(sctx)?;

    Ok(0)
}

pub fn object_ctrl(id: ObjID, cmd: ObjectControlCmd, arg: u64, arg2: u64) -> Result<u64> {
    let obj = lookup_object(id, LookupFlags::empty()).ok_or(TwzError::NOT_FOUND);
    match cmd {
        ObjectControlCmd::Sync => {
            crate::pager::sync_object(&obj?);
        }
        ObjectControlCmd::Delete(_) => {
            let obj = obj?;
            obj.mark_for_delete();
            // If this object is a registered security context, deleting it is the teardown
            // trigger: drop the registry entry so the context's unregister (and its region
            // sweep) actually runs, rather than waiting on the racy last-manager reap.
            crate::security::on_sctx_object_delete(id);
            // Just this object, not a scan of every object in the system: nothing else became
            // reapable by marking this one, and anything that becomes reapable later is caught
            // by the reaper thread, which the unmap paths poke.
            crate::obj::scan_deleted_one(&obj);
        }
        ObjectControlCmd::Preload => {
            let obj = obj
                .or_else(|_| crate::pager::lookup_object_and_wait(id).ok_or(TwzError::NOT_FOUND))?;
            {
                let guard = obj.lock_page_tables();
                let _ = crate::pager::ensure_in_core(
                    &obj,
                    guard,
                    &[(PageNumber::from_offset(0), MAX_SIZE / PageNumber::PAGE_SIZE)],
                    PagerFlags::PREFETCH,
                    true,
                    &mut false,
                    None,
                )?;
            }
            let tree = obj.lock_page_tables();
            let _ = obj
                .ensure_in_core(tree, PageNumber::meta_page(), 1, &mut false, &mut false)
                .inspect_err(|e| log::error!("failed to preload object {}: {}", id, e))?;
        }

        ObjectControlCmd::PreloadRange => {
            let obj = obj
                .or_else(|_| crate::pager::lookup_object_and_wait(id).ok_or(TwzError::NOT_FOUND))?;
            let nr = (arg2 as usize).min(MAX_PRELOAD_RANGES);
            let specs = unsafe { core::slice::from_raw_parts(arg as *const PreloadRangeSpec, nr) };
            const MAX_PAGES: usize = MAX_SIZE / PageNumber::PAGE_SIZE;
            let mut reqs = heapless::Vec::<_, MAX_PRELOAD_RANGES>::new();
            for spec in specs {
                let start = (spec.start_page as usize).min(MAX_PAGES);
                let nr_pages = (spec.nr_pages as usize).min(MAX_PAGES - start);
                if nr_pages == 0 {
                    continue;
                }
                let _ = reqs.push((
                    PageNumber::from_offset(start * PageNumber::PAGE_SIZE),
                    nr_pages,
                ));
            }
            if !reqs.is_empty() {
                let guard = obj.lock_page_tables();
                let _ = crate::pager::ensure_in_core(
                    &obj,
                    guard,
                    reqs.as_slice(),
                    PagerFlags::PREFETCH,
                    true,
                    &mut false,
                    None,
                )?;
            }
        }

        ObjectControlCmd::AddNote => {
            let obj = obj
                .or_else(|_| crate::pager::lookup_object_and_wait(id).ok_or(TwzError::NOT_FOUND))?;
            let value = unsafe { core::slice::from_raw_parts(arg as *const u8, arg2 as usize) };
            let key = obj.add_note(value);
            return Ok(key);
        }

        ObjectControlCmd::RemoveNote(key) => {
            let obj = obj
                .or_else(|_| crate::pager::lookup_object_and_wait(id).ok_or(TwzError::NOT_FOUND))?;
            obj.get_notes().remove(key);
        }

        ObjectControlCmd::GetNote(key) => {
            let obj = obj
                .or_else(|_| crate::pager::lookup_object_and_wait(id).ok_or(TwzError::NOT_FOUND))?;
            let buf = unsafe { core::slice::from_raw_parts_mut(arg as *mut u8, arg2 as usize) };
            if let Some(len) = obj.get_note(key, buf) {
                return Ok(len as u64);
            } else {
                return Err(TwzError::NOT_FOUND);
            }
        }

        ObjectControlCmd::EnumerateNotes(offset) => {
            let obj = obj
                .or_else(|_| crate::pager::lookup_object_and_wait(id).ok_or(TwzError::NOT_FOUND))?;
            let buf = unsafe { core::slice::from_raw_parts_mut(arg as *mut u64, arg2 as usize) };
            let keys = obj.enumerate_notes(offset as usize, buf.len());
            let len = keys.len().min(buf.len());
            buf[..len].copy_from_slice(&keys[..len]);
            return Ok(len as u64);
        }
        _ => {
            log::warn!(
                "object_ctrl: unimplemented command {:?} for object {} (arg={}, arg2={})",
                cmd,
                id,
                arg,
                arg2
            );
        }
    }
    Ok(0)
}

pub fn map_ctrl(start: usize, _len: usize, cmd: MapControlCmd, opts: u64) -> Result<u64> {
    let map = current_vmc()?
        .lookup_slot(start / MAX_SIZE)
        .ok_or(TwzError::INVALID_ARGUMENT)?;
    map.ctrl(cmd, opts)
}

pub fn sys_enumerate(arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> Result<usize> {
    let kind = EnumerateKind::try_from(arg0)?;
    let offset = arg3 as usize;

    // The buffer's element type is per-kind -- slot numbers, not object ids -- so it is built
    // inside each arm.
    match kind {
        EnumerateKind::Objects => {
            let buf = unsafe { create_user_slice(arg1, arg2).ok_or(TwzError::INVALID_ARGUMENT) }?;
            crate::obj::enumerate_objects(buf, offset)
        }
        EnumerateKind::Threads => {
            let buf = unsafe { create_user_slice(arg1, arg2).ok_or(TwzError::INVALID_ARGUMENT) }?;
            crate::thread::enumerate_objects(buf, offset)
        }
        EnumerateKind::MappedSlots => {
            let buf = unsafe { create_user_slice(arg1, arg2).ok_or(TwzError::INVALID_ARGUMENT) }?;
            current_vmc()?.enumerate_slots(buf, offset)
        }
    }
}

/// Copy ranges into, or zero ranges within, an object that already exists.
///
/// Same `object_source` shape as [`sys_object_create`]'s sources: a source with id 0 zeroes the
/// destination range, any other id copies from that object. Zeroing is the point of the call --
/// `zero_range` drops the frames under whole pages, which is the only way a page that faulted into
/// a live object is ever given back.
/// Call accounting for [`sys_object_copy`], printed once at shutdown.
///
/// Exists because the console is the wrong instrument for this question: `klog_println` from
/// userspace interleaves mid-line between threads, so a `grep -c` of a marker undercounts and can
/// read zero while the code ran -- which it did, twice, while chasing where the runtime's decommit
/// goes. One print, from one thread, at shutdown, cannot be corrupted that way.
///
/// It prints even when the count is zero. "Never called" is the informative outcome here, and a
/// counter that stays silent in that case is indistinguishable from one that failed to build.
pub mod copystats {
    use core::sync::atomic::{AtomicU64, Ordering};

    static CALLS: AtomicU64 = AtomicU64::new(0);
    static ZERO_SRCS: AtomicU64 = AtomicU64::new(0);
    static ZERO_BYTES: AtomicU64 = AtomicU64::new(0);
    static COPY_SRCS: AtomicU64 = AtomicU64::new(0);
    static ERRS: AtomicU64 = AtomicU64::new(0);

    pub fn call() {
        CALLS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn zero(len: u64) {
        ZERO_SRCS.fetch_add(1, Ordering::Relaxed);
        ZERO_BYTES.fetch_add(len, Ordering::Relaxed);
    }

    pub fn copy() {
        COPY_SRCS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn err() {
        ERRS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn print() {
        logln!(
            "== sys_object_copy: {} calls, {} zero srcs ({} KiB), {} copy srcs, {} errors ==",
            CALLS.load(Ordering::Relaxed),
            ZERO_SRCS.load(Ordering::Relaxed),
            ZERO_BYTES.load(Ordering::Relaxed) / 1024,
            COPY_SRCS.load(Ordering::Relaxed),
            ERRS.load(Ordering::Relaxed),
        );
    }
}

pub fn sys_object_copy(dest: ObjID, srcs: &[object_source]) -> Result<()> {
    // Every range has to stop short of the meta page, which is the object's last page. One bound
    // for three problems: the meta page holds the `MetaInfo` a content-derived id is computed
    // over, so writing it breaks `check_id` for every later mapper; offsets past the object's end
    // build page-table entries outside its range, silently; and a large enough offset reaches
    // `setup_zero_range`'s `VirtAddr::new(..).unwrap()` on a non-canonical address, which panics
    // the kernel rather than failing the call.
    const LIMIT: u64 = (MAX_SIZE - PageNumber::PAGE_SIZE) as u64;
    fn in_range(start: u64, len: u64) -> Result<()> {
        match start.checked_add(len) {
            Some(end) if end <= LIMIT => Ok(()),
            _ => Err(ArgumentError::InvalidArgument.into()),
        }
    }

    copystats::call();
    let obj = lookup_object(dest, LookupFlags::empty())
        .ok_or(ObjectError::NoSuchObject)
        .inspect_err(|_| copystats::err())?;
    for src in srcs {
        in_range(src.dest_start, src.len).inspect_err(|_| copystats::err())?;
        if src.id == 0 {
            copystats::zero(src.len);
            obj.zero_range(src.dest_start as usize, src.len as usize)
                .inspect_err(|e| {
                    copystats::err();
                    log::error!("failed to zero range in object {}: {}", dest, e)
                })?;
        } else {
            let src_id = ObjID::from(src.id);
            // Both copy paths take the two objects' page tables through `utils::lock_two`, which
            // asserts the two locks differ -- an object copying from itself would panic the kernel
            // instead of failing here.
            if src_id == dest {
                return Err(ArgumentError::InvalidArgument.into());
            }
            copystats::copy();
            in_range(src.src_start, src.len).inspect_err(|_| copystats::err())?;
            let so =
                lookup_object(src_id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
            so.copy_range(
                &obj,
                src.src_start as usize,
                src.dest_start as usize,
                src.len as usize,
            )
            .inspect_err(|e| log::error!("failed to copy range from object {}: {}", src_id, e))?;
        }
    }
    Ok(())
}
