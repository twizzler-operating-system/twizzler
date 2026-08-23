use alloc::{sync::Arc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
    usize,
};

use twizzler_abi::{
    arch::SLOTS,
    device::CacheType,
    object::{MAX_SIZE, ObjID, Protections},
    syscall::{MapControlCmd, MapFlags, ThreadSyncReference, ThreadSyncWake, TimeSpan},
    trace::{CONTEXT_FAULT, ContextFaultEvent, FaultFlags, TraceEntryFlags, TraceKind},
    upcall::MemoryAccessKind,
};
use twizzler_rt_abi::{
    bindings::{SYNC_FLAG_ASYNC_DURABLE, SYNC_FLAG_DURABLE},
    error::{ObjectError, ResourceError, TwzError},
};

use super::{
    PageFaultFlags, Slot,
    fault::{FaultClass, FaultStage, record_class, record_stage, stage_start},
};
use crate::{
    arch::VirtAddr,
    instant::Instant,
    memory::{
        context::{
            ContextRef, ObjectContextInfo,
            virtmem::regionmgr::{InsertGuard, RemoveGuard, SlotMgr},
        },
        pagetables::{MappingCursor, MappingFlags, MappingSettings},
    },
    mutex::Mutex,
    obj::{ObjectRef, PageNumber, PtGuard, pagetables::ObjectPageTable},
    security::PermsInfo,
    syscall::sync::wakeup,
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

/// Pages filled per anonymous fault. See [`MapRegion::fault_around`]; 1 restores one page per
/// fault.
///
/// **4 -> 16 on 2026-08-23** (`pageperf.md` §4). `page_fault_zero_fill` 811 -> 609 ns/touch,
/// **-24.9%, disjoint ranges**, against a drift of +0.1% measured by repeating the baseline arm
/// after the treatment. Mechanism gated on counts rather than the clock: faults per touch
/// 0.251 -> 0.063 (1/4 -> 1/16) and `PERFMARK-MAPFRAMES pages_per_call` 3.99 -> 15.96, so the runs
/// really do coalesce into one `map_frames` descent.
///
/// A third arm at 8 read 672 ns (-17.1%) against 677 predicted by fitting `ns/touch = F/N + P` to
/// the other two, which is what makes the model below trustworthy rather than a curve through two
/// points: **F ~= 1,077 ns fixed per fault, P ~= 542 ns per page**. At 16 the fixed term is 67
/// ns/touch and P dominates, so raising this further buys little (32 predicts 576, 64 predicts
/// 559) and `FILL_BATCH_MAX` is 16, so anything above that needs a second change. Attack P.
///
/// **The memory-pressure signature is not caused by this const**, though it looks like it. The
/// 16 arm ended with `idle=26,548` free frames and 1,014 allocation waits against the 4 arm's
/// `idle=471,728` and zero -- but the *repeated* 4 arm read `idle=27,301` and 255 waits, i.e. the
/// same signature on the unchanged const. It is boot-to-boot reclaim variance. Without the
/// repeated baseline this would have been attributed here and the change rejected.
///
/// What these benches cannot see: they touch every page in the window, so over-allocation on a
/// *sparse* first-touch workload is unmeasured. `fault_around` bounds the run by the 2 MiB block
/// and stops at a present neighbour, which limits it, but a sparse object still gets 16-page runs.
pub(crate) const ANON_FAULT_AROUND: usize = 16;

#[derive(Clone)]
pub struct MapRegion {
    pub object: ObjectRef,
    pub offset: u64,
    pub cache_type: CacheType,
    pub prot: Protections,
    pub flags: MapFlags,
    pub range: Range<VirtAddr>,
    pub stable: Option<Arc<Mutex<ObjectPageTable>>>,
    /// The object's default protections, from [`crate::obj::Object::check_id`] at insert time.
    /// Memoized there in a `Once`, so this is the same answer the fault path used to recompute per
    /// fault -- and that recomputation is two `Once` polls and a global counter on every fault.
    pub default_prot: Protections,
    /// Security context to install this mapping in; zero means the mapping thread's active one.
    pub target_sctx: ObjID,
    pub should_sync: Arc<AtomicBool>,
    /// Set once this region has been taken out of its [RegionManager] and unmapped. Shared across
    /// clones, since the fault path works from a clone taken before the removal.
    pub removed: Arc<AtomicBool>,
}

impl From<&MapRegion> for ObjectContextInfo {
    fn from(value: &MapRegion) -> Self {
        ObjectContextInfo {
            object: value.object.clone(),
            cache: value.cache_type,
            perms: value.prot,
            flags: value.flags,
            target_sctx: value.target_sctx,
        }
    }
}

/// An instruction fetch reached the fault handler for a region whose effective protections lack
/// EXEC. Rejecting these outright broke ~60% of runs, so this is observation only.
static EXEC_FAULT_NO_EXEC: crate::thread::locktrack::diag::Counter =
    crate::thread::locktrack::diag::Counter::new("exec fault on a region without EXEC");

impl MapRegion {
    fn trace_fault(
        &self,
        addr: VirtAddr,
        ip: VirtAddr,
        cause: MemoryAccessKind,
        pfflags: PageFaultFlags,
        used_pager: bool,
        large: bool,
        start_time: Instant,
    ) {
        if ip.is_kernel() || addr.is_kernel_object_memory() {
            return;
        }
        if TRACE_MGR.any_enabled(TraceKind::Context, CONTEXT_FAULT) {
            let mut flags = FaultFlags::empty();
            match cause {
                MemoryAccessKind::Read => flags.insert(FaultFlags::READ),
                MemoryAccessKind::Write => flags.insert(FaultFlags::WRITE),
                MemoryAccessKind::InstructionFetch => flags.insert(FaultFlags::EXEC),
            }
            if pfflags.contains(PageFaultFlags::USER) {
                flags.insert(FaultFlags::USER);
            }
            if large {
                flags.insert(FaultFlags::LARGE);
            }
            if used_pager {
                flags.insert(FaultFlags::PAGER);
            }

            let processing_time = Instant::now()
                .checked_sub_instant(&start_time)
                .map(|d| TimeSpan::from_nanos(d.as_nanos() as u64))
                .unwrap_or(TimeSpan::ZERO);
            let data = ContextFaultEvent {
                addr: addr.raw(),
                obj: self.object().id(),
                flags,
                processing_time,
            };
            let entry =
                new_trace_entry(TraceKind::Context, CONTEXT_FAULT, TraceEntryFlags::HAS_DATA);

            TRACE_MGR.enqueue(TraceEvent::new_with_data(entry, data));
        }
    }

    pub fn mapping_cursor(&self, start: usize, len: usize) -> MappingCursor {
        MappingCursor::new(self.range.start.offset(start).unwrap(), len)
    }

    pub fn mapping_settings(&self, wp: bool, is_kern_obj: bool) -> MappingSettings {
        let mut prot = self.prot;
        prot.insert(Protections::READ);
        if wp {
            prot.remove(Protections::WRITE);
        }
        MappingSettings::new(
            prot,
            self.cache_type,
            if is_kern_obj {
                MappingFlags::GLOBAL
            } else {
                MappingFlags::USER
            },
        )
    }

    pub fn object(&self) -> &ObjectRef {
        &self.object
    }

    /// Which run of pages to fill for a fault on `page`, as `(first, count, fills_faulting_page)`.
    ///
    /// One fault per page charges the whole fault path -- region lookup, security check, object
    /// page-table lock, ~6 us -- against a single 4 KiB zeroing, and an anonymous object written
    /// sequentially pays it for every page in turn. Filling a run amortizes that over
    /// `ANON_FAULT_AROUND` pages.
    ///
    /// Which run depends on what is already mapped on either side of the fault, since filling
    /// ahead of a backward walk allocates pages nothing will touch:
    ///
    /// | prev | next | run |
    /// |---|---|---|
    /// | absent | absent | forward -- first touch of a fresh region |
    /// | present | absent | forward -- a forward walk |
    /// | absent | present | backward -- a backward walk |
    /// | present | present | just this page -- filling a hole between mapped pages |
    ///
    /// The run then stops at the first page that is already there, so every page in it is one
    /// `ensure_in_core` will fill. Not just tidiness: `ensure_in_core` precharges frames only when
    /// the *first* page of the run is missing and allocates the rest from under the object's
    /// page-table lock, so a run starting on a present page would put a frame allocation --
    /// possibly a waiting one -- under that lock.
    ///
    /// Probes are `get_frame` lookups in the object's own tables, which the caller is already
    /// holding: at most `ANON_FAULT_AROUND + 1` of them, against a fault that costs microseconds.
    ///
    /// Runs are bounded to the enclosing 2 MiB block, the alignment the large-page and pager paths
    /// assume, and stop short of the meta page and the null page. Pager-backed objects are left
    /// alone: their read-ahead is built in `ensure_in_core_pager` out of what the pager returns.
    ///
    /// The third element says the faulting page was absent and so is one of the pages this run
    /// fills. `handle_fault` uses it to skip the COW check, which a frame allocated moments ago
    /// cannot need; `false` whenever the answer is not known here, which is the pre-existing
    /// behaviour.
    fn fault_around(
        &self,
        pt: &mut ObjectPageTable,
        page: PageNumber,
    ) -> (PageNumber, usize, bool) {
        const PAGES_PER_BLOCK: usize = 0x200000 / PageNumber::PAGE_SIZE;
        if ANON_FAULT_AROUND <= 1 || self.object.use_pager() {
            return (page, 1, false);
        }
        let mut present = |p: PageNumber| pt.get_frame(p.as_byte_offset() as u64).is_some();
        // The faulting page is already in the object's tables, so this fault is about the address
        // space rather than the fill. `handle_fault` decides whether to install the object-table
        // entry from whether *anything* got filled, so filling a neighbour here would make it skip
        // that and refault. Leave this fault exactly as it was before fault-around existed.
        if present(page) {
            return (page, 1, false);
        }
        let prev_present = page.prev().is_some_and(&mut present);
        let next_present = present(page.next());
        // Both neighbours mapped: a hole, not a run. Filling around it would allocate pages for an
        // access pattern that has already been served.
        if prev_present && next_present {
            return (page, 1, true);
        }

        let block_start = page.align_down(PAGES_PER_BLOCK).num();
        if next_present {
            // Backward: extend behind the fault, never onto the null page.
            let lowest = block_start
                .max(1)
                .max(page.num().saturating_sub(ANON_FAULT_AROUND - 1));
            let mut first = page.num();
            while first > lowest && !present((first - 1).into()) {
                first -= 1;
            }
            return (first.into(), page.num() - first + 1, true);
        }
        // Forward, starting at the fault itself.
        let highest = (block_start + PAGES_PER_BLOCK)
            .min(page.num() + ANON_FAULT_AROUND)
            .min(PageNumber::meta_page().num());
        // `next` was probed above and is absent on this branch, so start past it. The `max` keeps
        // the count at least one where the bounds leave no room (a fault on the meta page).
        let mut end = (page.num() + 2).min(highest).max(page.num() + 1);
        while end < highest && !present(end.into()) {
            end += 1;
        }
        (page, end - page.num(), true)
    }

    pub(super) fn handle_fault(
        &self,
        addr: VirtAddr,
        ip: VirtAddr,
        cause: MemoryAccessKind,
        pfflags: PageFaultFlags,
        start_time: Instant,
        perms: PermsInfo,
        sctxid: ObjID,
        map_ctx: ContextRef,
    ) -> Result<(), TwzError> {
        let page_number = PageNumber::from_address(addr);

        let is_kern_obj = addr.is_kernel_object_memory();

        log::trace!(
            "map fault for {:?} at {:?} (page {}) in object {} (ip: {:?}): pn {}, is_kern {}",
            cause,
            addr,
            page_number,
            self.object().id(),
            ip,
            page_number,
            is_kern_obj
        );

        let mut used_pager = false;
        let mut all_were_present = true;
        // Whether the faulting page itself was absent and has just been filled. Such a page is
        // backed by a frame this thread allocated moments ago, so it cannot be a COW mapping.
        let mut filled_fault_page = false;
        let needs_fill = !pfflags.contains(PageFaultFlags::PRESENT);
        let t_pt = stage_start();
        let mut obj_page_tree = match self.stable.as_ref() {
            // A stable region's clone holds only what the object held when it was taken, and
            // nothing refills it afterwards, so a page missing from it has to be brought into the
            // object and re-shared here -- `ensure_in_core` drops and retakes the object's own
            // lock internally, so it cannot be handed the clone's. Object tables before the clone,
            // the order MapControlCmd::Discard uses.
            Some(stable) if needs_fill => {
                let mut pt = self.object.lock_page_tables();
                record_stage(FaultStage::PtLock, t_pt);
                let t = stage_start();
                pt = self.object.ensure_in_core(
                    pt,
                    page_number,
                    1,
                    &mut used_pager,
                    &mut all_were_present,
                )?;
                record_stage(FaultStage::EnsureCore, t);
                let mut clone = PtGuard::new(stable);
                let offset = page_number.as_byte_offset() as u64;
                pt.setup_cow_range(&mut clone, offset, offset, PageNumber::PAGE_SIZE)?;
                // Unavoidably nested, unlike the two-guard sites that use `release_two`: `clone` is
                // this block's value and has to outlive `pt`, so `pt`'s shootdown wait runs with
                // the clone's lock held. Stated rather than left to drop order -- the exposure is
                // one wait, and closing it would mean restructuring the fault path's return.
                drop(pt);
                clone
            }
            Some(stable) => {
                let pt = PtGuard::new(stable);
                record_stage(FaultStage::PtLock, t_pt);
                pt
            }
            None => {
                let mut pt = self.object.lock_page_tables();
                record_stage(FaultStage::PtLock, t_pt);
                if needs_fill {
                    let t = stage_start();
                    let (first, count, fills) = self.fault_around(&mut pt, page_number);
                    filled_fault_page = fills;
                    pt = self.object.ensure_in_core(
                        pt,
                        first,
                        count,
                        &mut used_pager,
                        &mut all_were_present,
                    )?;
                    record_stage(FaultStage::EnsureCore, t);
                }
                pt
            }
        };
        if used_pager {
            record_class(FaultClass::Pager);
        }
        let prot = perms.effective(self.default_prot, self.prot);

        log::trace!(
            "fault info: addr={:?} cause={:?} flags={:?} ip={:?} page_number={} used_pager={} all_were_present={} prot={:?}",
            addr,
            cause,
            pfflags,
            ip,
            page_number,
            used_pager,
            all_were_present,
            prot
        );

        // For a PRESENT fault `ensure_in_core` above is skipped, so `all_were_present` stays true,
        // and the tail of this function re-installs the *same* protections and returns Ok -- so a
        // fault those protections cannot satisfy makes the instruction retry and fault identically,
        // forever. Nothing bounds that: `send_upcall`'s MAX_UPCALLS_WITHOUT_RETURN covers only the
        // Err path. One wedge was observed at 8.4M identical instruction fetches on one address,
        // against a slot whose object had been unmapped and the slot reused.
        //
        // `check_settings` above encodes the permission rule but returns early when the active sctx
        // is 0, as does `check_security`; `effective()` then caps at the region's own `map_prots`,
        // so with sctx 0 nothing between the fault and here enforces it.
        //
        // Reported, NOT rejected. Returning Err here -- the obvious symmetry with the write check
        // below -- failed ~60% of runs against a ~3% baseline, so an exec fault reaching this point
        // without EXEC in `prot` is evidently routine and satisfied by something further down,
        // rather than being the livelock by itself. What distinguishes the wedge is that the fault
        // *repeats*; `log_fault`'s refault counter is what identifies that. This probe exists to
        // show which regions and protections reach here at all, and whether the looping one differs
        // from the rest.
        if cause == MemoryAccessKind::InstructionFetch
            && !prot.contains(Protections::EXEC)
            && EXEC_FAULT_NO_EXEC.hit()
        {
            emerglogln!(
                "exec fault without EXEC: addr {:?} ip {:?} prot {:?} region-prot {:?} present {} object {}",
                addr,
                ip,
                prot,
                self.prot,
                pfflags.contains(PageFaultFlags::PRESENT),
                self.object().id(),
            );
        }

        let mut did_cow = false;
        if cause == MemoryAccessKind::Write {
            if !prot.contains(Protections::WRITE) {
                log::error!(
                    "write fault at addr {:?} (ip: {:?}) with prot {:?} in object {}",
                    addr,
                    ip,
                    prot,
                    self.object().id()
                );
                // TODO
                return Err(ObjectError::MapFailed.into());
            }
            // Skipped when the fill above allocated this page's frame: `cow_at` would walk the
            // tables, precharge an allocator, and run a consistency pass only to find a mapping
            // that was never shared. That is ~700 ns on the majority of write faults.
            if !filled_fault_page {
                let t = stage_start();
                did_cow = obj_page_tree.maybe_cow_at(page_number.as_byte_offset() as u64, false)?;
                record_stage(FaultStage::Cow, t);
                if did_cow {
                    record_class(FaultClass::Cow);
                }
            }
            log::trace!(
                "cow at page {} in object {} due to write fault at addr {:?} (ip: {:?}): {} use_pager: {}",
                page_number,
                self.object().id(),
                addr,
                ip,
                did_cow,
                self.object().use_pager()
            );
        }

        if all_were_present && !did_cow {
            log::trace!(
                "fault: all pages were present in object {} page {} (addr {:?}) (flags = {:?})",
                self.object().id(),
                page_number,
                addr,
                pfflags
            );
            // This region is a clone taken before the regions lock was dropped, so it may have been
            // unmapped since. remove_object publishes that under the same page tables we hold here,
            // so checking now is what stops a fault from re-installing a mapping that was just torn
            // down -- which would leave the object reachable after unmap and its map count raised
            // with nothing left to lower it. Returning Ok refaults, and finds no region.
            if self.removed.load(Ordering::SeqCst) {
                self.trace_fault(addr, ip, cause, pfflags, used_pager, false, start_time);
                return Ok(());
            }
            let cursor = MappingCursor::new(self.range.start, self.range.end - self.range.start);
            // TODO: is this always user?
            let settings = MappingSettings::new(prot, self.cache_type, MappingFlags::USER);
            let t = stage_start();
            let mapped = map_ctx.ensure_object_mapped(
                sctxid,
                // `obj_page_tree` is this region's stable clone when it has one, and a clone
                // takes no count against the object; see `VirtContext::map_object`.
                self.stable.is_none().then(|| &**self.object()),
                cursor,
                &mut obj_page_tree,
                settings,
            );
            record_stage(FaultStage::MapObject, t);
            if mapped {
                record_class(FaultClass::Mapped);
            }
        }

        self.trace_fault(addr, ip, cause, pfflags, used_pager, false, start_time);
        Ok(())
    }

    pub fn invalidate(&self) {
        if let Some(stable) = self.stable.as_ref() {
            let mut stable = PtGuard::new(stable);
            stable.invalidate(self.offset, self.range.end - self.range.start);
        } else {
            self.object()
                .lock_page_tables()
                .invalidate(self.offset, self.range.end - self.range.start);
        }
    }

    pub fn ctrl(&self, cmd: MapControlCmd, _opts: u64) -> Result<u64, TwzError> {
        match cmd {
            MapControlCmd::Sync(sync_info_ptr) => {
                let mut pt = if let Some(stable) = self.stable.as_ref() {
                    PtGuard::new(stable)
                } else {
                    self.object().lock_page_tables()
                };
                if sync_info_ptr.is_null() {
                    let dirty_pages = pt.get_dirty_and_reset()?;
                    log::trace!(
                        "sync region {:?} with dirty pages {:?}",
                        self.range,
                        dirty_pages
                    );
                    drop(pt);
                    if self.object().use_pager() && !dirty_pages.is_empty() {
                        crate::pager::sync_region(self, dirty_pages, None, 0, false);
                    }
                } else {
                    let sync_info = unsafe { sync_info_ptr.read() };
                    let version = sync_info.release_compare;

                    if sync_info.flags & SYNC_FLAG_DURABLE != 0 {
                        let dirty_pages = pt.get_dirty_and_reset()?;
                        log::trace!(
                            "sync region {:?} with dirty pages {:?}",
                            self.range,
                            dirty_pages
                        );
                        drop(pt);
                        if self.object().use_pager() && !dirty_pages.is_empty() {
                            crate::pager::sync_region(
                                self,
                                dirty_pages,
                                Some(sync_info),
                                version,
                                sync_info.flags & SYNC_FLAG_ASYNC_DURABLE != 0,
                            );
                        }
                    }

                    if sync_info.flags & SYNC_FLAG_ASYNC_DURABLE != 0 {
                        if sync_info.flags & SYNC_FLAG_DURABLE == 0 {
                            self.should_sync.store(true, Ordering::SeqCst);
                        }
                        if !sync_info.release_ptr.is_null() {
                            unsafe { sync_info.try_release() }?;
                            let wake = ThreadSyncWake::new(
                                ThreadSyncReference::Virtual(sync_info.release_ptr.cast()),
                                usize::MAX,
                            );
                            wakeup(&wake)?;
                        }
                    }
                }

                Ok(0)
            }
            MapControlCmd::Invalidate => {
                self.invalidate();
                Ok(0)
            }
            MapControlCmd::Discard | MapControlCmd::Update => {
                if let Some(stable) = self.stable.as_ref() {
                    let mut pt = self.object().lock_page_tables();
                    let mut stable = PtGuard::new(stable);
                    let len = self.range.end - self.range.start;
                    stable.setup_zero_range(self.offset, len)?;
                    pt.setup_cow_range(&mut *stable, self.offset, self.offset, len)?;
                    // Both locks off before either one's shootdown wait runs. Letting these drop
                    // implicitly would run the inner guard's wait under the outer lock, which is
                    // the nested hold this change exists to remove.
                    PtGuard::release_two(pt, stable);
                }
                self.invalidate();
                Ok(0)
            }
        }
    }
}

/// Regions are handed out as `Arc`s rather than cloned.
///
/// The fault path takes one per fault, and a `MapRegion` clone is four `Arc` bumps (`object`,
/// `stable`, `should_sync`, `removed`) and four matching drops. One refcount does the same job.
///
/// Every operation takes `&self`. What used to be one context-wide sleeping mutex -- taken on
/// every fault, and measured convoying from 155 ns to 7.5 us per fault at smp4 -- is now a shard
/// spinlock per slot inside [SlotMgr], plus a per-slot state that carries the exclusion an insert
/// or a remove needs across its mapping work. See [SlotState] there.
pub struct RegionManager {
    /// User slots, based at zero.
    user: SlotMgr,
    /// Kernel-object slots. Separate because they start around 2^34 (`KOBJ_START / MAX_SIZE`),
    /// which no two-level table based at zero can reach.
    kobj: SlotMgr,
}

impl Default for RegionManager {
    fn default() -> Self {
        let kobj_start = VirtAddr::start_kernel_object_memory();
        let kobj_end = VirtAddr::end_kernel_object_memory();
        Self {
            // `SLOTS`, not the width of the user half: it is the ABI's count on both arches and so
            // is what userspace can name, and it sidesteps `end_user_memory` being exclusive on
            // amd64 and inclusive on aarch64.
            user: SlotMgr::new(0, SLOTS),
            kobj: SlotMgr::new(
                kobj_start.raw() as usize / MAX_SIZE,
                (kobj_end.raw() - kobj_start.raw()) as usize / MAX_SIZE,
            ),
        }
    }
}

impl RegionManager {
    /// Which manager answers for `slot`, if either. `None` is an ordinary miss: the fault path
    /// looks user slots up in the kernel context as a fallback.
    fn mgr_for(&self, slot: Slot) -> Option<&SlotMgr> {
        [&self.user, &self.kobj]
            .into_iter()
            .find(|mgr| mgr.contains(slot.raw()))
    }

    pub fn lookup_region(&self, slot: Slot) -> Option<Arc<MapRegion>> {
        self.mgr_for(slot)?.lookup(slot.raw())
    }

    /// Claim `slot` for a mapping that is about to be built. See [SlotMgr::begin_insert].
    pub fn begin_insert(&self, slot: Slot) -> Result<InsertGuard<'_>, TwzError> {
        self.mgr_for(slot)
            .ok_or(ResourceError::OutOfResources)?
            .begin_insert(slot.raw())
    }

    /// Take the region out of `slot`, holding it against reuse until the teardown finishes. See
    /// [SlotMgr::begin_remove].
    pub fn begin_remove(&self, slot: Slot) -> Option<(Arc<MapRegion>, RemoveGuard<'_>)> {
        self.mgr_for(slot)?.begin_remove(slot.raw())
    }

    /// Every region in this context. Cold path: `unregister_sctx` needs a *complete* list, since a
    /// region missed there never gets its `dec_map_count` and its object is never reaped.
    pub fn mappings(&self) -> Vec<Arc<MapRegion>> {
        let mut out = Vec::with_capacity(self.mapping_count());
        self.user.for_each(|_, region| out.push(region));
        self.kobj.for_each(|_, region| out.push(region));
        out
    }

    /// Approximate; see [SlotMgr]'s count field.
    pub fn mapping_count(&self) -> usize {
        self.user.count() + self.kobj.count()
    }

    /// The objects mapped in this context, deduplicated. Debug-only, and derived from the walk
    /// above rather than tracked, since nothing hot asks.
    pub fn objects(&self) -> Vec<ObjID> {
        let mut ids = self
            .mappings()
            .iter()
            .map(|region| region.object.id())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}
