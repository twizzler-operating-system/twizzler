use alloc::{sync::Arc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
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
    fault::{FaultClass, FaultStage, census, record_class, record_stage, stage_start},
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

/// Whether [`ANON_FAULT_AROUND`] is a ceiling that adapts per region, or a fixed width.
/// `false` restores the fixed behaviour from one tree, which is the A/B.
pub(crate) const ADAPTIVE_FAULT_AROUND: bool = false;

/// Stream ends remembered per region. Four covers the contended zero-fill bench's threads without
/// making the miss path a scan.
pub(crate) const FA_STREAMS: usize = 8;

/// Empty slot. `0` cannot mean this: page 0 is a real page number, and a region whose slots all
/// read zero would score its first fault as a miss and halve the window before it has any evidence
/// at all. `compartment_spawn_exit` creates many short-lived regions and paid that on nearly every
/// one.
const FA_EMPTY: u64 = u64::MAX;

/// Pages COW'd per write fault. 1 restores one page per fault -- the behaviour before this existed.
///
/// The COW path is the fill path's unbatched sibling: `ANON_FAULT_AROUND` amortises a fault's fixed
/// cost over 16 pages, while every COW fault pays its own precharge, descent and TLB shootdown.
/// A `--diag` boot (`cowmeas`) puts **55.5% of all faults** in the COW class (361,540 of 651,885)
/// and **17.5% of total fault time** in the `cow` stage, with the pager at 0.0% -- so this is
/// in-memory page-table latency, which batching can attack.
///
/// **SHIPPED at 8 (2026-08-26).** Three-arm A/B/A `cowA1`/`cowB`/`cowA2`, one tree state, clean
/// consts, quiet box; spawnbench.md §66-68:
///
///                       1 (off)    8       1 (off)   drift   effect
///     faults/spawn        118.3    67.3     118.3    0.00%   -43.1%
///     flushes/spawn       120.1    57.5     120.7    0.42%   -52.3%
///     shootdowns/spawn     13.3    12.2      13.5    1.27%    -9.3%
///     spawn_exit          2.551ms  2.395    2.615    2.49%    -7.28%
///
/// The identical arms differ by **4 events in 248,518** on faults, which is why counts were the
/// primary signal: the wall-clock floor on the same arms is 2.49%. Shootdowns fall *less* than
/// faults proportionally (-9.3% vs -43%) because the `is_full()` break caps runs by design and much
/// of the shootdown traffic is unmap/teardown this does not touch -- two populations, one
/// escalation site. Measured post-TLBFIX / post-pipe-EOF(`also`) / post-predicate-alignment; that
/// tree state is **unobtainable** now, so re-baseline rather than comparing forward to these.
///
/// **Starts at 8, not 16, and deliberately.** A speculative COW is more expensive than a
/// speculative zero-fill: it burns a frame *and* a 4 KiB copy, where an over-eager anon fill burns
/// only the frame. The fitted model behind `ANON_FAULT_AROUND` (`perf-inprogress.md`: ns/touch =
/// F/N + P over three arms) says 8 already captures most of the fixed-cost win, so 8 buys the bulk
/// of the benefit at half the waste. Note F and P themselves are absolutes from a possibly-armed
/// round and are not used here to predict a saving -- only the *shape* is.
pub(crate) const COW_FAULT_AROUND: usize = 8;

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
    pub should_sync: AtomicBool,
    /// Adaptive fault-around width for this region, and the page each recent batch ended at.
    ///
    /// A fixed [`ANON_FAULT_AROUND`] cannot serve both workloads it meets: on dense first-touch it
    /// is worth 4.4x on `page_fault_zero_fill` and 25% on `compartment_spawn_exit`, and on a
    /// sparse one -- an on-target cargo build, whose 1 MiB stacker stacks are touched a few
    /// pages at a time -- it materialises 8x the pages the workload ever reads (1.56M against
    /// 194k), all of them zeroed for nothing.
    ///
    /// The signal needs no accessed bits: with a batch of N installed, a *sequential* first touch
    /// faults again exactly where the last batch ended, and a scattered one does not. So the
    /// position of the next fault says whether the last batch was consumed.
    ///
    /// Several slots, not one, because `page_fault_zero_fill_contended` is several threads
    /// streaming through the same region at once. Their faults interleave, so a single
    /// "expected next" would read every one of them as scattered and collapse the window on
    /// precisely the bench this is meant to protect.
    pub fa_window: AtomicU32,
    pub fa_streams: [AtomicU64; FA_STREAMS],
    /// Next `fa_streams` slot to replace.
    pub fa_slot: AtomicU32,
    /// Set once this region has been taken out of its [RegionManager] and unmapped. Plain fields
    /// rather than their own `Arc`s: regions are only ever shared as `Arc<MapRegion>` (the fault
    /// path holds one taken before the removal), so the enclosing refcount already carries them.
    pub removed: AtomicBool,
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
    /// Fold this fault into the region's window: continuing a recorded stream means the last batch
    /// was walked through, anything else means it may have been installed for nothing.
    ///
    /// Halve on a miss rather than dropping to 1: a single stray fault inside an otherwise
    /// sequential pass should cost width, not the whole window.
    fn fa_note(&self, page: PageNumber) -> usize {
        if !ADAPTIVE_FAULT_AROUND {
            return ANON_FAULT_AROUND;
        }
        let want = page.num() as u64;
        let mut hit = false;
        let mut any = false;
        for slot in self.fa_streams.iter() {
            let v = slot.load(Ordering::Relaxed);
            if v != FA_EMPTY {
                any = true;
                if v == want {
                    hit = true;
                    break;
                }
            }
        }
        let cur = self.fa_window.load(Ordering::Relaxed).max(1) as usize;
        // No stream recorded yet is not evidence of a scattered access pattern -- it is a region
        // nobody has faulted twice. Shrinking here charges every fresh region a halving.
        let next = if hit || !any {
            (cur * 2).min(ANON_FAULT_AROUND)
        } else {
            (cur / 2).max(1)
        };
        self.fa_window.store(next as u32, Ordering::Relaxed);
        next
    }

    /// Record where a batch ended, so the next fault that continues it is recognised. Slots are
    /// replaced round-robin by the low bits of the page number: no lock, and two streams landing
    /// on one slot costs a shrink, not correctness.
    fn fa_record(&self, end: usize) {
        if !ADAPTIVE_FAULT_AROUND {
            return;
        }
        // Round-robin on an insertion counter, not a function of the address: keying on `end`
        // made two threads streaming through the same span collide on one slot and evict each
        // other, which is `page_fault_zero_fill_contended` exactly.
        let slot = self.fa_slot.fetch_add(1, Ordering::Relaxed) as usize % FA_STREAMS;
        self.fa_streams[slot].store(end as u64, Ordering::Relaxed);
    }

    fn fault_around(
        &self,
        pt: &mut ObjectPageTable,
        page: PageNumber,
    ) -> (PageNumber, usize, bool) {
        const PAGES_PER_BLOCK: usize = 0x200000 / PageNumber::PAGE_SIZE;
        if ANON_FAULT_AROUND <= 1 || self.object.use_pager() {
            return (page, 1, false);
        }
        let width = self.fa_note(page);
        if width <= 1 {
            self.fa_record(page.num() + 1);
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
            self.fa_record(page.num() + 1);
            return (page, 1, true);
        }

        let block_start = page.align_down(PAGES_PER_BLOCK).num();
        if next_present {
            // Backward: extend behind the fault, never onto the null page.
            let lowest = block_start.max(1).max(page.num().saturating_sub(width - 1));
            let mut first = page.num();
            while first > lowest && !present((first - 1).into()) {
                first -= 1;
            }
            // Backward runs end at the fault, so that is where a forward continuation resumes.
            self.fa_record(page.num() + 1);
            return (first.into(), page.num() - first + 1, true);
        }
        // Forward, starting at the fault itself.
        let highest = (block_start + PAGES_PER_BLOCK)
            .min(page.num() + width)
            .min(PageNumber::meta_page().num());
        // `next` was probed above and is absent on this branch, so start past it. The `max` keeps
        // the count at least one where the bounds leave no room (a fault on the meta page).
        let mut end = (page.num() + 2).min(highest).max(page.num() + 1);
        while end < highest && !present(end.into()) {
            end += 1;
        }
        self.fa_record(end);
        (page, end - page.num(), true)
    }

    /// Choose a run of pages to COW together, given a write fault on `page`.
    ///
    /// Mirrors [`Self::fault_around`]'s shape but inverts its central test. `fault_around` extends
    /// over pages that are **absent** (it is about to fill them); this extends over pages that are
    /// **present**, because a COW candidate is by definition a page that already exists in the
    /// object's tables and faulted on permission. That inversion is the whole reason COW gets no
    /// batching today: `fault_around` returns `(page, 1, false)` the moment it sees the faulting
    /// page present, which is every COW fault.
    ///
    /// Bounded by the same 2 MiB block, and stops at the first absent neighbour. Pages that are
    /// present but not shared cost `cow_at` a walk that finds nothing to do (~700 ns by the note at
    /// its call site) -- wasteful but correct -- so the run stops on absence rather than trying to
    /// read shared-ness here, which would mean a second descent per candidate to save one.
    fn cow_around(&self, pt: &mut ObjectPageTable, page: PageNumber) -> usize {
        const PAGES_PER_BLOCK: usize = 0x200000 / PageNumber::PAGE_SIZE;
        if COW_FAULT_AROUND <= 1 || self.object.use_pager() {
            return 1;
        }
        let mut present = |p: PageNumber| pt.get_frame(p.as_byte_offset() as u64).is_some();
        if !present(page) {
            // Not a COW candidate at all; leave the fault exactly as it was.
            return 1;
        }
        let block_start = page.align_down(PAGES_PER_BLOCK).num();
        let highest = (block_start + PAGES_PER_BLOCK)
            .min(page.num() + COW_FAULT_AROUND)
            .min(PageNumber::meta_page().num());
        let mut end = (page.num() + 1).min(highest).max(page.num() + 1);
        while end < highest && present(end.into()) {
            end += 1;
        }
        end - page.num()
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
                pt = self.object.ensure_in_core_fault(
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
                    pt = self.object.ensure_in_core_fault(
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
                let cow_run = self.cow_around(&mut *obj_page_tree, page_number);
                did_cow = obj_page_tree.maybe_cow_range(
                    page_number.as_byte_offset() as u64,
                    cow_run,
                    false,
                )?;
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

        let mut mapped = false;
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
            mapped = map_ctx.ensure_object_mapped(
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

        if census::enabled() {
            // Evaluated in the order a fault resolves, so each fault lands in exactly one bucket
            // and the columns sum to the total.
            let kind = if used_pager {
                census::Kind::Pager
            } else if did_cow {
                census::Kind::Cow
            } else if needs_fill && !all_were_present {
                census::Kind::Fill
            } else if mapped {
                census::Kind::MapOnly
            } else {
                census::Kind::Present
            };
            census::record(self.object().id(), kind);
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

    /// Take this region's dirty pages and hand them to the pager, returning whether anything was
    /// submitted.
    ///
    /// The page tables consulted are the region's own when it is STABLE and the object's
    /// otherwise, which is the distinction that makes a whole-system sweep ([`SysCtrlCmd::SyncAll`]
    /// in `syscall::sys_sysctrl`) need this rather than the object-keyed background path: a STABLE
    /// mapping's dirty bits live in its private COW clone and nothing keyed by object can see them.
    ///
    /// `wait` blocks until the pager acknowledges instead of submitting and returning.
    pub fn sync_dirty(&self, wait: bool) -> Result<bool, TwzError> {
        let mut pt = if let Some(stable) = self.stable.as_ref() {
            PtGuard::new(stable)
        } else {
            self.object().lock_page_tables()
        };
        let dirty_pages = pt.get_dirty_and_reset()?;
        log::trace!(
            "sync region {:?} with dirty pages {:?}",
            self.range,
            dirty_pages
        );
        // Before the submit, not after: `sync_region` can block on the pager.
        drop(pt);
        if self.object().use_pager() && !dirty_pages.is_empty() {
            crate::pager::sync_region(self, dirty_pages, None, 0, wait);
            return Ok(true);
        }
        Ok(false)
    }

    pub fn ctrl(&self, cmd: MapControlCmd, _opts: u64) -> Result<u64, TwzError> {
        match cmd {
            MapControlCmd::Sync(sync_info_ptr) => {
                if sync_info_ptr.is_null() {
                    self.sync_dirty(false)?;
                    return Ok(0);
                }
                let mut pt = if let Some(stable) = self.stable.as_ref() {
                    PtGuard::new(stable)
                } else {
                    self.object().lock_page_tables()
                };
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

/// Regions are handed out as `Arc`s rather than cloned; `MapRegion` is not `Clone` at all.
///
/// The fault path takes one per fault. A value clone used to be four `Arc` bumps and four drops;
/// now the enclosing `Arc<MapRegion>` refcount is the whole cost, and `should_sync`/`removed`
/// are plain fields (two fewer allocations per map).
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
