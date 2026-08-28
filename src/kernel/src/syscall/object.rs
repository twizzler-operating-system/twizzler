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
    memory::context::{Context, ContextRef, virtmem::Slot},
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
/// `NO_CURRENT_THREAD` shows post-boot windows with no current thread do occur.
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

/// Where `sys_object_create` spends its time, stage by stage.
///
/// Off in the tree for the reason F11 documents: two clock reads and a u128 conversion per stage,
/// on a path `sysbench`'s `object_create_delete` measures. Turned on only for an attribution run.
/// Stage split of `sys_object_ctrl(Delete)`, mirroring [`createprofile`].
///
/// `Scan` **contains** `Reap`: `scan_deleted_one` tests reapability and only then reaps, and the
/// difference between those two is the question -- an object that is still mapped is not reapable,
/// so the delete is cheap here and the real cost is deferred to the reaper thread. Reporting only
/// a combined figure would make a deferred teardown and an inline one look the same.
pub mod deleteprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::instant::Instant;

    pub const DELETE_PROFILE: bool = false;

    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Stage {
        /// `lookup_object`: the global id map.
        Lookup = 0,
        /// `mark_for_delete`: `record_delete` plus a `fetch_or`.
        Mark,
        /// `scan_deleted_one`: the reapability test, and `Reap` when it passes.
        Scan,
        /// `reap`: the actual teardown. Only reached when the object is reapable *now*.
        Reap,
        Total,
    }

    pub const NR: usize = Stage::Total as usize + 1;
    pub const NAMES: [&str; NR] = ["lookup", "mark", "scan", "reap", "TOTAL"];

    static COUNT: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
    static NS: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];

    #[inline(always)]
    pub fn start() -> Instant {
        if DELETE_PROFILE {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    pub fn record(stage: Stage, start: Instant) {
        if !DELETE_PROFILE {
            return;
        }
        let dur: twizzler_abi::syscall::TimeSpan = (Instant::now() - start).into();
        COUNT[stage as usize].fetch_add(1, Ordering::Relaxed);
        NS[stage as usize].fetch_add(dur.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Per-stage (count, nanoseconds), cumulative, for [`crate::perfmark`] to difference.
    pub fn snapshot() -> [(u64, u64); NR] {
        let mut out = [(0u64, 0u64); NR];
        if !DELETE_PROFILE {
            return out;
        }
        for i in 0..NR {
            out[i] = (
                COUNT[i].load(Ordering::Relaxed),
                NS[i].load(Ordering::Relaxed),
            );
        }
        out
    }
}

pub mod createprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::instant::Instant;

    pub const CREATE_PROFILE: bool = false;

    /// A/B switch for the create-path fast paths, as a bundle: installing the meta page directly
    /// rather than through the generic fill path (see the call site in `sys_object_create`), and
    /// skipping the id hash in `note_written_meta` when the verdict is already recorded. Both are
    /// pure removals of work whose result was discarded; the const exists so one tree state can
    /// build both arms.
    pub const OBJ_CREATE_FASTPATHS: bool = true;

    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Stage {
        /// `new_nonce`: the CSPRNG under its global mutex.
        Nonce = 0,
        /// `calculate_new_id`: one sha256 over 48 bytes (two in debug, via the `debug_assert`).
        Id,
        /// `Object::new` plus the `Arc`.
        New,
        /// The `srcs` loop: zero/copy ranges into the new object.
        Srcs,
        /// `write_meta`, i.e. the generic fill path down to the object's last page.
        Meta,
        /// `register_object`: the global id map.
        Register,
        /// Within [Stage::New]: the `Box<[(AtomicU64, AtomicU64); NUM_DEVICE_INTERRUPTS]>`.
        NewDevBox,
        /// Within [Stage::New]: the rest of the struct literal.
        NewStruct,
        /// Within [Stage::New]: `Arc::new`, i.e. the heap allocation plus the move.
        NewArc,
        /// Within [Stage::Meta]: `alloc_frame(ZEROED | WAIT_OK)`.
        MetaFrame,
        /// Within [Stage::Meta]: `add_frame`, which builds the object page tables down to the
        /// meta page.
        MetaAdd,
        /// Within [Stage::Meta]: `note_written_meta`.
        MetaNote,
        Total,
    }

    pub const NR: usize = Stage::Total as usize + 1;
    pub const NAMES: [&str; NR] = [
        "nonce",
        "id",
        "new",
        "srcs",
        "meta",
        "register",
        "new_devbox",
        "new_struct",
        "new_arc",
        "meta_frame",
        "meta_add",
        "meta_note",
        "TOTAL",
    ];

    static COUNT: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
    static NS: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];

    #[inline(always)]
    pub fn start() -> Instant {
        if CREATE_PROFILE {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    pub fn record(stage: Stage, start: Instant) {
        if !CREATE_PROFILE {
            return;
        }
        let dur: twizzler_abi::syscall::TimeSpan = (Instant::now() - start).into();
        COUNT[stage as usize].fetch_add(1, Ordering::Relaxed);
        NS[stage as usize].fetch_add(dur.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Per-stage (count, nanoseconds), cumulative, for [`crate::perfmark`] to difference.
    pub fn snapshot() -> [(u64, u64); NR] {
        let mut out = [(0u64, 0u64); NR];
        if !CREATE_PROFILE {
            return out;
        }
        for i in 0..NR {
            out[i] = (
                COUNT[i].load(Ordering::Relaxed),
                NS[i].load(Ordering::Relaxed),
            );
        }
        out
    }

    pub fn print() {
        if !CREATE_PROFILE {
            return;
        }
        let total = COUNT[Stage::Total as usize].load(Ordering::Relaxed);
        if total == 0 {
            return;
        }
        logln!(
            "== sys_object_create profile: {} calls, size_of::<Object>() = {} ==",
            total,
            core::mem::size_of::<crate::obj::Object>(),
        );
        for (i, name) in NAMES.iter().enumerate() {
            let c = COUNT[i].load(Ordering::Relaxed);
            if c == 0 {
                continue;
            }
            let ns = NS[i].load(Ordering::Relaxed);
            logln!(
                "  {:>9}: {} calls, {} ns/call, {} us total",
                name,
                c,
                ns / c,
                ns / 1000
            );
        }
    }
}

pub fn sys_object_create(
    create: &ObjectCreate,
    srcs: &[object_source],
    ties: &[object_tie],
) -> Result<ObjID> {
    use createprofile::Stage;
    let t_total = createprofile::start();
    let t = createprofile::start();
    let nonce = if create.flags.contains(ObjectCreateFlags::NO_NONCE) {
        0
    } else {
        new_nonce()?
    };
    createprofile::record(Stage::Nonce, t);
    let t = createprofile::start();
    let id = calculate_new_id(create.kuid, MetaFlags::default(), nonce, create.def_prot);
    createprofile::record(Stage::Id, t);
    let t = createprofile::start();
    let inner = Object::new(id, create.lt, ties);
    let t_arc = createprofile::start();
    let obj = Arc::new(inner);
    createprofile::record(Stage::NewArc, t_arc);
    // Nothing is on the store until the first sync, whatever sources were copied in here -- those
    // land in pages, not on disk. Recording zero rather than leaving it unknown is what lets the
    // fault path fill a brand-new object without a single pager round trip.
    obj.set_known_len(0);
    // We just derived the ID from these fields, so the check `check_id` would do is already done.
    // Recording it now saves the first mapper of this object a `read_meta`, which for a
    // pager-backed object is a page-in (mapperf.md).
    obj.set_verified_id(true, create.def_prot);
    createprofile::record(Stage::New, t);
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
        createprofile::record(Stage::Total, t_total);
        return Ok(obj.id());
    }
    let t = createprofile::start();
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
    createprofile::record(Stage::Srcs, t);
    let t = createprofile::start();
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
    if src_wrote_meta || !createprofile::OBJ_CREATE_FASTPATHS {
        while !obj.write_meta(meta) {
            log::error!("failed to write object metadata -- retrying");
        }
    } else {
        obj.init_meta(meta);
    }
    createprofile::record(Stage::Meta, t);
    log::trace!(
        "sys_object_create: create={:?}, srcs={}, ties={}: {:?}",
        create,
        srcs.len(),
        ties.len(),
        obj.id(),
    );
    let t = createprofile::start();
    crate::obj::register_object(obj.clone());
    createprofile::record(Stage::Register, t);
    // Deliberately not an immediate delete: `object_ctrl`'s Delete marks the object *and* runs
    // scan_deleted() inline, and a brand-new object has no mappings and no pins, so it was
    // reap-eligible before its creator could map it. The flag means "delete on last unmap", so
    // record that and let the unmap path decide.
    if create.flags.contains(ObjectCreateFlags::DELETE) {
        obj.set_delete_on_last_unmap();
    }
    createprofile::record(Stage::Total, t_total);
    Ok(obj.id())
}

/// Where `sys_object_map` spends its time, split at the one boundary that matters.
///
/// `SPACESTAT` measures the monitor's map gate at 103-146 us per miss and `insert_object` accounts
/// for 4.9 us of it, so ~96% of the syscall is somewhere else. The only other thing it does is find
/// the object, and for an object the kernel has never seen that is a pager round trip
/// (`lookup_object_and_wait`). This is the counter that settles it rather than inferring it from
/// the arithmetic (`INPROG.md`, next step 1).
pub mod mapstats {
    use core::sync::atomic::{AtomicU64, Ordering};

    pub use crate::memory::context::virtmem::mapprofile::MAP_STATS;

    static CALLS: AtomicU64 = AtomicU64::new(0);
    static PAGER: AtomicU64 = AtomicU64::new(0);
    /// Resolving the caller's memory context, before any object work. Small by inspection, and
    /// measured for exactly that reason: the three segments here have to add up to the syscall, or
    /// the missing ~110 us is somewhere none of them looked.
    static PRE_NS: AtomicU64 = AtomicU64::new(0);
    static LOOKUP_HIT_NS: AtomicU64 = AtomicU64::new(0);
    static LOOKUP_PAGER_NS: AtomicU64 = AtomicU64::new(0);
    static LOOKUP_PAGER_MAX: AtomicU64 = AtomicU64::new(0);
    static INSERT_NS: AtomicU64 = AtomicU64::new(0);

    /// Bucket bounds in ns for the cold (pager) lookup; the last bucket is everything above.
    ///
    /// A mean cannot distinguish "every call got slower" from "some calls were rescued by a
    /// periodic timer at a constant cost", and those imply different bugs. The bounds straddle the
    /// two modes actually observed (fast ~306-362 us, slow ~1.17-1.44 ms) so a bimodal boot shows
    /// up as two populated buckets rather than being inferred from an average of the two.
    const LOOKUP_BUCKET_NS: [u64; 4] = [100_000, 500_000, 1_000_000, 2_000_000];
    const NR_LOOKUP_BUCKETS: usize = LOOKUP_BUCKET_NS.len() + 1;
    static LOOKUP_PAGER_HIST: [AtomicU64; NR_LOOKUP_BUCKETS] =
        [const { AtomicU64::new(0) }; NR_LOOKUP_BUCKETS];

    /// A clock read the profile will actually use, or nothing. See [`MAP_PROFILE`].
    #[inline(always)]
    pub fn stamp() -> crate::instant::Instant {
        if MAP_STATS {
            crate::instant::Instant::now()
        } else {
            crate::instant::Instant::zero()
        }
    }

    #[inline(always)]
    pub fn delta_ns(from: crate::instant::Instant, to: crate::instant::Instant) -> u64 {
        if MAP_STATS {
            (to - from).as_nanos() as u64
        } else {
            0
        }
    }

    pub fn pre(ns: u64) {
        if !MAP_STATS {
            return;
        }
        PRE_NS.fetch_add(ns, Ordering::Relaxed);
    }

    pub fn lookup(ns: u64, used_pager: bool) {
        if !MAP_STATS {
            return;
        }
        CALLS.fetch_add(1, Ordering::Relaxed);
        if used_pager {
            PAGER.fetch_add(1, Ordering::Relaxed);
            LOOKUP_PAGER_NS.fetch_add(ns, Ordering::Relaxed);
            LOOKUP_PAGER_MAX.fetch_max(ns, Ordering::Relaxed);
            let bucket = LOOKUP_BUCKET_NS
                .iter()
                .position(|b| ns < *b)
                .unwrap_or(NR_LOOKUP_BUCKETS - 1);
            LOOKUP_PAGER_HIST[bucket].fetch_add(1, Ordering::Relaxed);
        } else {
            LOOKUP_HIT_NS.fetch_add(ns, Ordering::Relaxed);
        }
    }

    pub fn insert(ns: u64) {
        if !MAP_STATS {
            return;
        }
        INSERT_NS.fetch_add(ns, Ordering::Relaxed);
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        if calls == 0 {
            return;
        }
        let pager = PAGER.load(Ordering::Relaxed);
        let hits = calls - pager;
        logln!(
            "== sys_object_map: {} calls, {} reached the pager ==",
            calls,
            pager,
        );
        logln!(
            "  lookup: hit {} ns/call over {}, pager {} ns/call over {} (max {} us); total {} us",
            LOOKUP_HIT_NS.load(Ordering::Relaxed) / hits.max(1),
            hits,
            LOOKUP_PAGER_NS.load(Ordering::Relaxed) / pager.max(1),
            pager,
            LOOKUP_PAGER_MAX.load(Ordering::Relaxed) / 1000,
            (LOOKUP_HIT_NS.load(Ordering::Relaxed) + LOOKUP_PAGER_NS.load(Ordering::Relaxed))
                / 1000,
        );
        // Only when there were cold lookups to bucket. An all-zero histogram for a boot that took
        // no samples is indistinguishable from a boot whose samples all landed in bucket 0, and
        // reporting a plausible-looking row for absent data is the failure mode this instrument
        // exists to avoid rather than reproduce.
        if pager > 0 {
            let mut hist = [0u64; NR_LOOKUP_BUCKETS];
            for (i, h) in hist.iter_mut().enumerate() {
                *h = LOOKUP_PAGER_HIST[i].load(Ordering::Relaxed);
            }
            logln!(
                "  lookup pager hist (<100us, <500us, <1ms, <2ms, 2ms+): {} {} {} {} {}",
                hist[0],
                hist[1],
                hist[2],
                hist[3],
                hist[4],
            );
        }
        logln!(
            "  map into context: {} ns/call, {} us total; context lookup {} ns/call",
            INSERT_NS.load(Ordering::Relaxed) / calls,
            INSERT_NS.load(Ordering::Relaxed) / 1000,
            PRE_NS.load(Ordering::Relaxed) / calls,
        );
    }
}

pub fn sys_object_map(
    id: ObjID,
    slot: usize,
    prot: Protections,
    handle: Option<ObjID>,
    flags: MapFlags,
    target_sctx: ObjID,
) -> Result<usize> {
    let entered = mapstats::stamp();
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(handle).ok_or(ObjectError::NoSuchObject)?
    } else {
        current_vmc()?
    };
    let start = mapstats::stamp();
    mapstats::pre(mapstats::delta_ns(entered, start));
    let mut used_pager = false;
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
        _ => {
            used_pager = true;
            match crate::pager::lookup_object_and_wait(id) {
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
            }
        }
    };
    let found = mapstats::stamp();
    mapstats::lookup(mapstats::delta_ns(start, found), used_pager);
    // Before the mapping, not after: the point is to have the pager working while the rest of the
    // syscall runs. Submission is a lock and a queue push; nothing here waits.
    crate::pager::prefetch_on_map(&obj);
    // TODO
    let _res =
        crate::operations::map_object_into_context(slot, obj, vm, prot.into(), flags, target_sctx);
    mapstats::insert(mapstats::delta_ns(found, mapstats::stamp()));
    Ok(slot)
}

pub fn sys_object_unmap(handle: Option<ObjID>, slot: usize) -> Result<u64> {
    use crate::memory::context::virtmem::unmapprofile::Initiator;
    let (vm, initiator) = if let Some(handle) = handle {
        (
            get_vmcontext_from_handle(handle).ok_or(ArgumentError::BadHandle)?,
            Initiator::Handle,
        )
    } else {
        (current_vmc()?, Initiator::Own)
    };
    vm.remove_object_from(
        Slot::try_from(slot).map_err(|_| ArgumentError::InvalidArgument)?,
        initiator,
    );
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

/// Switch for `STATPROF`: where `sys_object_stat`'s time goes, split lookup / info.
///
/// The syscall measures **30.5 us** against 117 ns for a trivial one, and
/// `HandleMgr::gc_handles` calls it once per tracked compartment on every handle insert and
/// remove. `Object::info` calls `count_pages`, which walks a cursor over `max_len()` -- the
/// object's whole 1 GiB range -- so the suspicion is that a "stat" is O(object range). Counted
/// rather than inferred.
use core::sync::atomic::{AtomicU64, Ordering};

pub const STAT_PROFILE: bool = false;
static STAT_CALLS: AtomicU64 = AtomicU64::new(0);
static STAT_LOOKUP_NS: AtomicU64 = AtomicU64::new(0);
static STAT_INFO_NS: AtomicU64 = AtomicU64::new(0);
/// Pages the walk actually found. Discriminates O(pages) -- fix by keeping a counter -- from
/// O(range) -- fix by making the reader skip empty subtrees.
static STAT_PAGES: AtomicU64 = AtomicU64::new(0);

pub fn sys_object_info(handle: ObjID) -> Result<ObjectInfo> {
    if !STAT_PROFILE {
        let obj = crate::obj::lookup_object(handle, LookupFlags::empty())
            .ok_or(ObjectError::NoSuchObject)?;
        return Ok(obj.info());
    }
    let t0 = crate::instant::Instant::now();
    let obj =
        crate::obj::lookup_object(handle, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
    let t1 = crate::instant::Instant::now();
    let info = obj.info();
    let t2 = crate::instant::Instant::now();
    STAT_PAGES.fetch_add(info.pages as u64, Ordering::Relaxed);
    let n = STAT_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    let l = STAT_LOOKUP_NS.fetch_add((t1 - t0).as_nanos() as u64, Ordering::Relaxed);
    let i = STAT_INFO_NS.fetch_add((t2 - t1).as_nanos() as u64, Ordering::Relaxed);
    if n % 8192 == 0 {
        logln!(
            "STATPROF calls={} lookup={}ns/call info={}ns/call pages={}/call (info is count_pages over max_len)",
            n,
            l / n,
            i / n,
            STAT_PAGES.load(Ordering::Relaxed) / n
        );
    }
    Ok(info)
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
    let t_total = deleteprofile::start();
    let t_lookup = deleteprofile::start();
    let obj = lookup_object(id, LookupFlags::empty()).ok_or(TwzError::NOT_FOUND);
    match cmd {
        ObjectControlCmd::Sync => {
            crate::pager::sync_object(&obj?);
        }
        ObjectControlCmd::Delete(_) => {
            deleteprofile::record(deleteprofile::Stage::Lookup, t_lookup);
            let obj = obj?;
            let t = deleteprofile::start();
            obj.mark_for_delete();
            // If this object is a registered security context, deleting it is the teardown
            // trigger: drop the registry entry so the context's unregister (and its region
            // sweep) actually runs, rather than waiting on the racy last-manager reap.
            crate::security::on_sctx_object_delete(id);
            deleteprofile::record(deleteprofile::Stage::Mark, t);
            let t = deleteprofile::start();
            if crate::obj::TARGETED_REAP {
                // Just this object, not a scan of every object in the system: nothing else became
                // reapable by marking this one, and anything that becomes reapable later is caught
                // by the reaper thread, which the unmap paths poke.
                crate::obj::scan_deleted_one(&obj);
            } else {
                crate::obj::scan_deleted();
            }
            deleteprofile::record(deleteprofile::Stage::Scan, t);
            deleteprofile::record(deleteprofile::Stage::Total, t_total);
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
