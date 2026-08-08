use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
    usize,
};

use nonoverlapping_interval_tree::NonOverlappingIntervalTree;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    syscall::{MapControlCmd, MapFlags, ThreadSyncReference, ThreadSyncWake, TimeSpan},
    trace::{CONTEXT_FAULT, ContextFaultEvent, FaultFlags, TraceEntryFlags, TraceKind},
    upcall::{MemoryAccessKind, MemoryContextViolationInfo, UpcallInfo},
};
use twizzler_rt_abi::{
    bindings::{SYNC_FLAG_ASYNC_DURABLE, SYNC_FLAG_DURABLE},
    error::{ObjectError, TwzError},
};

use super::PageFaultFlags;
use crate::{
    arch::VirtAddr,
    instant::Instant,
    memory::{
        FAULT_STATS,
        context::{ObjectContextInfo, kernel_context},
        pagetables::{MappingCursor, MappingFlags, MappingSettings},
    },
    mutex::Mutex,
    obj::{ObjectRef, PageNumber, pagetables::ObjectPageTable},
    security::PermsInfo,
    syscall::sync::wakeup,
    thread::{current_memory_context, current_thread_ref},
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

#[derive(Clone)]
pub struct MapRegion {
    pub object: ObjectRef,
    pub offset: u64,
    pub cache_type: CacheType,
    pub prot: Protections,
    pub flags: MapFlags,
    pub range: Range<VirtAddr>,
    pub stable: Option<Arc<Mutex<ObjectPageTable>>>,
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
        }
    }
}

fn check_settings(
    addr: VirtAddr,
    settings: &MappingSettings,
    kind: MemoryAccessKind,
) -> Result<(), UpcallInfo> {
    if !settings.flags().contains(MappingFlags::USER) {
        return Ok(());
    }
    if current_thread_ref().is_some_and(|ct| ct.secctx.active_id().raw() == 0) {
        return Ok(());
    }
    let upcall =
        UpcallInfo::MemoryContextViolation(MemoryContextViolationInfo::new(addr.raw(), kind));
    match kind {
        MemoryAccessKind::Read => {
            if !settings.perms().contains(Protections::READ) {
                return Err(upcall);
            }
        }
        MemoryAccessKind::Write => {
            if !settings.perms().contains(Protections::WRITE) {
                return Err(upcall);
            }
        }
        MemoryAccessKind::InstructionFetch => {
            if !settings.perms().contains(Protections::EXEC) {
                return Err(upcall);
            }
        }
    }
    Ok(())
}

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

    pub(super) fn handle_fault(
        &self,
        addr: VirtAddr,
        ip: VirtAddr,
        cause: MemoryAccessKind,
        pfflags: PageFaultFlags,
        start_time: Instant,
        perms: PermsInfo,
        default_prot: Protections,
        sctxid: ObjID,
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
        FAULT_STATS.count[0].fetch_add(1, Ordering::SeqCst);

        let mut used_pager = false;
        let mut all_were_present = true;
        let mut obj_page_tree = if let Some(stable) = self.stable.as_ref() {
            stable.lock()
        } else {
            self.object.lock_page_tables()
        };
        let prot = perms.effective(default_prot, self.prot);
        if !pfflags.contains(PageFaultFlags::PRESENT) && self.stable.is_none() {
            obj_page_tree = self.object.ensure_in_core(
                obj_page_tree,
                page_number,
                1,
                &mut used_pager,
                &mut all_were_present,
            )?;
        }

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
            did_cow = obj_page_tree.maybe_cow_at(page_number.as_byte_offset() as u64, false)?;
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
            let ctx = current_memory_context().unwrap_or_else(|| kernel_context().clone());
            // TODO: is this always user?
            let settings = MappingSettings::new(prot, self.cache_type, MappingFlags::USER);
            ctx.ensure_object_mapped(sctxid, cursor, &mut obj_page_tree, settings);
        }

        self.trace_fault(addr, ip, cause, pfflags, used_pager, false, start_time);
        Ok(())
    }

    pub fn invalidate(&self) {
        if let Some(stable) = self.stable.as_ref() {
            let mut stable = stable.lock();
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
                    stable.lock()
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
                    let mut stable = stable.lock();
                    let len = self.range.end - self.range.start;
                    stable.setup_zero_range(self.offset, len)?;
                    pt.setup_cow_range(&mut *stable, self.offset, self.offset, len)?;
                }
                self.invalidate();
                Ok(0)
            }
        }
    }
}

#[derive(Default)]
pub struct RegionManager {
    tree: NonOverlappingIntervalTree<VirtAddr, MapRegion>,
    objects: BTreeMap<ObjID, Vec<Range<VirtAddr>>>,
}

impl RegionManager {
    pub fn insert_region(&mut self, region: MapRegion) {
        let object_entry = self.objects.entry(region.object.id()).or_default();
        let range = region.range.clone();
        let old = self.tree.insert_replace(range.clone(), region);
        for old_region in old {
            let pos = object_entry
                .iter()
                .position(|item| item == &old_region.0)
                .expect("failed to find object range");
            object_entry.swap_remove(pos);
        }
        object_entry.push(range);
    }

    pub fn remove_region(&mut self, addr: VirtAddr) -> Option<MapRegion> {
        if let Some(region) = self.tree.remove(&addr) {
            let object_entry = self.objects.entry(region.object.id()).or_default();
            let pos = object_entry
                .iter()
                .position(|item| item == &region.range)
                .expect("failed to find object range");
            object_entry.swap_remove(pos);
            Some(region)
        } else {
            None
        }
    }

    pub fn lookup_region(&mut self, addr: VirtAddr) -> Option<&MapRegion> {
        self.tree.get(&addr)
    }

    pub fn object_mappings(&mut self, id: ObjID) -> impl Iterator<Item = &MapRegion> {
        self.objects.entry(id).or_default().iter().map(|info| {
            self.tree
                .get(&info.start)
                .expect("failed to lookup mapping")
        })
    }

    pub fn mappings(&self) -> impl Iterator<Item = &MapRegion> {
        self.tree.iter().map(|x| x.1.value())
    }

    pub fn objects(&self) -> impl Iterator<Item = &ObjID> {
        self.objects.keys().into_iter()
    }
}
