//! This mod implements [UserContext] and [KernelMemoryContext] for virtual memory systems.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{marker::PhantomData, mem::size_of, ops::Range, ptr::NonNull, sync::atomic::AtomicBool};

use region::{MapRegion, RegionManager};
use twizzler_abi::{
    device::CacheType,
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID, Protections},
    syscall::{MapControlCmd, MapFlags},
};
use twizzler_rt_abi::error::{ResourceError, TwzError};

use super::{
    KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, UserContext, kernel_context,
};
use crate::{
    arch::{
        address::VirtAddr,
        context::{ArchContext, ArchContextTarget},
    },
    idcounter::{Id, IdCounter, StableId},
    memory::{
        PhysAddr,
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        pagetables::{
            ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings,
            PhysAddrProvider, PhysMapInfo, Table, UninitPageProvider,
        },
        tracker::{FrameAllocFlags, FrameAllocator, take_or_new_frame_allocator},
    },
    mutex::Mutex,
    obj::{ObjectRef, PageNumber, pagetables::ObjectPageTable},
    once::Once,
    processor::{mp::current_processor, tls_ready},
    security::KERNEL_SCTX,
    spinlock::Spinlock,
    thread::current_thread_ref,
};

pub mod fault;
pub mod region;
mod tests;

pub use fault::page_fault;

/// A type that implements [super::Context] for virtual memory systems.
pub struct VirtContext {
    secctx: Mutex<BTreeMap<ObjID, ArchContext>>,
    /// The kernel context's arch state, held outside `secctx` because the kernel has exactly one
    /// security context: there is no map to consult and no lock to take. `Some` here is what makes
    /// this the kernel context.
    ///
    /// Not merely an optimization. Kernel heap growth reaches [`VirtContext::with_arch`] from
    /// inside the allocator's critical section -- ferroc's base allocator calls `allocate_chunk`,
    /// which calls [`GlobalPageAlloc::extend`] -- and `secctx` is a *sleeping* mutex, so taking it
    /// there is the `cannot lock mutex in critical context` panic that `stabilitybugs.md` calls
    /// Mode C.
    kernel_arch: Option<ArchContext>,
    // We keep a cache of the actual switch targets so that we don't need to take the above mutex
    // during switch_to. Unfortunately, it's still kinda hairy, since this is a spinlock of a
    // memory-allocating collection. See register_sctx for details.
    target_cache: Spinlock<BTreeMap<ObjID, ArchContextTarget>>,
    regions: Mutex<RegionManager>,
    id: Id<'static>,
    is_kernel: bool,
}

/// The kernel context's page-table root, cached at boot so that the thread-switch path can reach
/// it without taking any lock. See [`VirtContext::switch_to_kernel_context`].
static KERNEL_ARCH_TARGET: Once<ArchContextTarget> = Once::new();

static CONTEXT_IDS: IdCounter = IdCounter::new();

struct KernelSlotCounter {
    cur_kernel_slot: usize,
    kernel_slots_nums: Vec<Slot>,
}

static KERNEL_SLOT_COUNTER: Once<Mutex<KernelSlotCounter>> = Once::new();

fn kernel_slot_counter() -> &'static Mutex<KernelSlotCounter> {
    KERNEL_SLOT_COUNTER.call_once(|| {
        Mutex::new(KernelSlotCounter {
            cur_kernel_slot: Slot::try_from(VirtAddr::start_kernel_object_memory())
                .unwrap()
                .raw(),
            kernel_slots_nums: Vec::new(),
        })
    })
}

/// A representation of a slot number.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct Slot(usize);

impl Slot {
    fn start_vaddr(&self) -> VirtAddr {
        VirtAddr::new((self.0 * MAX_SIZE) as u64).unwrap()
    }

    fn raw(&self) -> usize {
        self.0
    }

    fn range(&self) -> Range<VirtAddr> {
        self.start_vaddr()..self.start_vaddr().offset(MAX_SIZE).unwrap()
    }
}

impl TryFrom<usize> for Slot {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let vaddr = VirtAddr::new((value * MAX_SIZE) as u64).map_err(|_| ())?;
        vaddr.try_into()
    }
}

impl TryFrom<VirtAddr> for Slot {
    type Error = ();

    fn try_from(value: VirtAddr) -> Result<Self, Self::Error> {
        if value.is_kernel() && !value.is_kernel_object_memory() {
            Err(())
        } else {
            Ok(Self(value.raw() as usize / MAX_SIZE))
        }
    }
}

const MAX_OPP_VEC: usize = 128;
struct ObjectPageProvider {
    pos: usize,
    inner_pos: usize,
    pages: heapless::Vec<(FrameRef, MappingSettings), MAX_OPP_VEC>,
}

impl ObjectPageProvider {
    pub fn new(pages: heapless::Vec<(FrameRef, MappingSettings), MAX_OPP_VEC>) -> Self {
        Self {
            pages,
            pos: 0,
            inner_pos: 0,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages
            .iter()
            .skip(self.pos)
            .fold(0, |acc, x| acc + x.0.nr_pages())
            - self.inner_pos / PageNumber::PAGE_SIZE
    }
}

impl PhysAddrProvider for ObjectPageProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        let page = self.pages.get(self.pos)?;
        if page.0.nr_pages() > 1 {
            log::trace!(
                "peek: {:?}",
                page.0.start_address().offset(self.inner_pos).unwrap()
            );
        }
        Some(PhysMapInfo {
            addr: page.0.start_address().offset(self.inner_pos).unwrap(),
            len: PageNumber::PAGE_SIZE * page.0.nr_pages() - self.inner_pos,
            settings: page.1,
        })
    }

    fn consume(&mut self, mut len: usize) {
        if len > PageNumber::PAGE_SIZE {
            if len / PageNumber::PAGE_SIZE >= 512 {
                log::trace!("consume: {:?} ({} pages)", len, len / PageNumber::PAGE_SIZE);
            }
        }
        while len > 0 && self.pos < self.pages.len() {
            let rem_len =
                PageNumber::PAGE_SIZE * self.pages[self.pos].0.nr_pages() - self.inner_pos;
            if len < rem_len {
                self.inner_pos += len;
                break;
            } else {
                len = len.saturating_sub(rem_len);
                self.pos += 1;
                self.inner_pos = 0;
            }
        }
    }
}

static ALL_CONTEXTS: Once<Mutex<BTreeMap<u64, Arc<VirtContext>>>> = Once::new();

fn get_all_contexts() -> &'static Mutex<BTreeMap<u64, Arc<VirtContext>>> {
    ALL_CONTEXTS.call_once(|| Mutex::new(BTreeMap::new()))
}

pub fn with_each_context(cb: impl FnMut(&Arc<VirtContext>)) {
    let all = get_all_contexts();
    let contexts = {
        let contexts = all.lock();
        contexts.values().cloned().collect::<Vec<_>>()
    };
    contexts.iter().for_each(cb);
}

impl VirtContext {
    fn __new(kernel_arch: Option<ArchContext>) -> Self {
        let mut secctx = Mutex::new(BTreeMap::new());
        // We ensure that the BTree never changes while we hold the lock.
        secctx.set_safe_with_spinlocks(true);
        let new = Self {
            regions: Mutex::new(RegionManager::default()),
            is_kernel: kernel_arch.is_some(),
            id: CONTEXT_IDS.next(),
            secctx,
            kernel_arch,
            target_cache: Spinlock::new(BTreeMap::new()),
        };
        new
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Arc<Self> {
        let this = Arc::new(Self::__new(Some(ArchContext::new_kernel())));
        let target = this.arch().target;
        // Built outside the spinlock and swapped in, for the reason `register_sctx` gives: filling
        // the map allocates, and `target_cache` is a spinlock.
        let mut targets = BTreeMap::new();
        targets.insert(KERNEL_SCTX, target);
        core::mem::swap(&mut *this.target_cache.lock(), &mut targets);
        // Cache the root now, while we're safely outside the thread-switch path.
        KERNEL_ARCH_TARGET.call_once(|| target);
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), this.clone());
        this
    }

    /// Switch the calling processor to the kernel page tables.
    ///
    /// Deliberately lock-free, because the thread-switch path calls this: going through
    /// `switch_to` would take `target_cache`, nesting a spinlock acquisition inside the switch
    /// (which the lock tracker's single intent slot cannot represent, `locktrack.rs:192`) and
    /// serialising every processor that goes idle on one lock. A no-op during very early boot,
    /// before the kernel context exists -- there is nothing to switch away from yet.
    pub fn switch_to_kernel_context() {
        let Some(target) = KERNEL_ARCH_TARGET.poll() else {
            return;
        };
        let proc = tls_ready().then(current_processor);
        // Safety: the kernel context's root outlives every thread, and is never freed.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    /// Construct a new context for userspace.
    pub fn new() -> Arc<Self> {
        let this = Arc::new(Self::__new(None));
        // TODO: remove this once we have full support for user security contexts
        this.register_sctx(KERNEL_SCTX, ArchContext::new());
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), this.clone());
        this
    }

    /// The kernel context's one arch context. Panics on a user context; see
    /// [`VirtContext::kernel_arch`].
    fn arch(&self) -> &ArchContext {
        self.kernel_arch
            .as_ref()
            .expect("not the kernel context: its arch contexts live in `secctx`")
    }

    /// This context's arch state for `sctx`, without a lock, if there is only one of them.
    fn single_arch(&self, sctx: ObjID) -> Option<&ArchContext> {
        let arch = self.kernel_arch.as_ref()?;
        // Any other sctx on the kernel context missed the (empty) map before this existed, so
        // falling through to it keeps that answer -- `None` from `try_with_arch`, the `expect` from
        // `with_arch` -- rather than handing back the kernel's tables for something else. No caller
        // does this today; the assert is there to say so if one starts.
        debug_assert_eq!(sctx, KERNEL_SCTX);
        (sctx == KERNEL_SCTX).then_some(arch)
    }

    /// Run `cb` against every arch context this context owns: the kernel's single one, or a user
    /// context's one per attached security context.
    fn for_each_arch(&self, mut cb: impl FnMut(&ArchContext)) {
        if let Some(arch) = self.kernel_arch.as_ref() {
            cb(arch);
            return;
        }
        for arch in self.secctx.lock().values() {
            cb(arch);
        }
    }

    pub fn try_with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> Option<R> {
        if let Some(arch) = self.single_arch(sctx) {
            return Some(cb(arch));
        }
        let secctx = self.secctx.lock();
        secctx.get(&sctx).map(|arch| cb(arch))
    }

    pub fn with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> R {
        if let Some(arch) = self.single_arch(sctx) {
            return cb(arch);
        }
        let secctx = self.secctx.lock();
        cb(secctx
            .get(&sctx)
            .expect("cannot get arch mapper for unattached security context"))
    }

    pub fn map_object(&self, info: &MapRegion) {
        // An explicit target wins; zero means "whatever this thread is running as", which for the
        // monitor is KERNEL_SCTX -- its instance id is zero too. That now resolves (see
        // `security::kernel_sctx`), so those mappings get installed here rather than left to the
        // fault path.
        let sctx = if self.is_kernel {
            // The kernel context has exactly one arch context, registered under KERNEL_SCTX by
            // `new_kernel`, and nothing ever registers another into it. Taking the caller's active
            // sctx here would just make `try_with_arch` miss and silently install nothing --
            // which is what every `insert_kernel_object` from a thread in a real context did.
            KERNEL_SCTX
        } else if info.target_sctx.raw() != 0 {
            info.target_sctx
        } else {
            current_thread_ref()
                .map(|ct| ct.active_sctx_id())
                .unwrap_or(KERNEL_SCTX)
        };

        let len = info.range.end - info.range.start;
        let cursor = MappingCursor::new(info.range.start, len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
            FrameAllocFlags::WAIT_OK,
        );
        // Reading the thread's own `secctx.active()` instead of `get_sctx(active_id())` is faster
        // (68% of this function). The two used to differ -- `get_sctx(0)` returned `Err` and
        // skipped this whole block -- but both now resolve to the single `kernel_sctx()`, so the
        // swap is available if this shows up in a profile again. See pagerperf.md 17.
        if let Ok(sctx) = crate::security::get_sctx(sctx) {
            let perms = sctx.lookup(info.object().id(), info.default_prot);
            let mut pt = if info.stable.is_some() {
                info.stable.as_ref().unwrap().lock()
            } else {
                info.object.lock_page_tables()
            };
            self.try_with_arch(sctx.id(), |arch| {
                pt.add_invalidate(arch.target, cursor);
                let settings = MappingSettings::new(
                    perms.effective(info.default_prot, info.prot),
                    info.cache_type,
                    MappingFlags::USER,
                );
                arch.object_map(cursor, &mut *pt, settings, &mut fa);
            });
        };
    }

    pub fn ensure_object_mapped(
        &self,
        sctxid: ObjID,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
    ) -> bool {
        // Ask before charging for it. Every fault on an already-resident page reaches here, and
        // only a couple of percent of them install anything, so the frame allocator below was
        // mostly being taken, precharged, and put back to discover there was nothing to do.
        if self.try_with_arch(sctxid, |arch| arch.is_object_mapped(cursor, settings)) == Some(true)
        {
            return false;
        }
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
            FrameAllocFlags::WAIT_OK,
        );
        self.with_arch(sctxid, |arch| {
            object_tables.add_invalidate(arch.target, cursor);
            arch.ensure_object_mapped(cursor, object_tables, settings, &mut fa)
        })
    }

    pub fn print_objects(&self) {
        let mut slots = self.regions.lock();
        for obj in slots.objects().copied().collect::<Vec<_>>().iter() {
            log!("{} => ", obj);
            for mapping in slots.object_mappings(*obj) {
                log!("{:?}, ", mapping.range);
            }
            logln!("");
        }
    }

    pub fn register_sctx(&self, sctx: ObjID, arch: ArchContext) {
        if self.kernel_arch.is_some() {
            // The kernel context's one arch context is a field, installed when it was built.
            debug_assert_eq!(sctx, KERNEL_SCTX);
            return;
        }
        let mut secctx = self.secctx.lock();
        if secctx.contains_key(&sctx) {
            return;
        }
        secctx.insert(sctx, arch);
        // Rebuild the target cache. We have to do it this way because we cannot allocate
        // memory while holding the target_cache lock (as it's a spinlock).
        let mut new_target_cache = BTreeMap::new();
        for value in secctx.iter() {
            new_target_cache.insert(*value.0, value.1.target);
        }
        // Swap out the target caches, dropping the old one after the spinlock is released.
        {
            let mut target_cache = self.target_cache.lock();
            core::mem::swap(&mut *target_cache, &mut new_target_cache);
        }
    }

    pub fn unregister_sctx(&self, sctx: ObjID) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut secctx = self.secctx.lock();
        if !secctx.contains_key(&sctx) {
            return;
        }
        let arch = secctx.remove(&sctx);

        // Retire the target *before* `arch` is dropped at the end of this function, not after: the
        // drop frees the root page table and releases the PCID, and until the cache is rebuilt a
        // concurrent `switch_to` on this sctx would still find the old target and load it. Doing it
        // here shrinks that window from "the whole unmap walk below" to "a switch that already read
        // the target", and keeps a recycled PCID from being installed against a freed root -- which
        // would alias one address space's translations onto whoever gets the PCID next. Same
        // build-then-swap dance as register_sctx, because target_cache is a spinlock and the
        // allocation has to happen outside it.
        let mut new_target_cache = BTreeMap::new();
        for value in secctx.iter() {
            new_target_cache.insert(*value.0, value.1.target);
        }
        {
            let mut target_cache = self.target_cache.lock();
            core::mem::swap(&mut *target_cache, &mut new_target_cache);
        }
        drop(secctx);
        drop(new_target_cache);

        if let Some(arch) = arch {
            let regions = self.regions.lock();
            let capacity = regions.mappings().count();
            drop(regions);

            let mut region_list = alloc::vec::Vec::<Arc<MapRegion>>::with_capacity(capacity);
            let regions = self.regions.lock();
            for region in regions.mappings() {
                if region.range.start.raw() == 0 {
                    continue;
                }
                region_list.push(region.clone());
            }
            drop(regions);

            for region in region_list {
                let cursor = region.mapping_cursor(0, MAX_SIZE);
                // Whichever tree backs this region, as in remove_object: a stable clone still has
                // to be told its mapping is gone, even though it never took a count against the
                // object and so has nothing to give back.
                let mut pt = if let Some(stable) = region.stable.as_ref() {
                    stable.lock()
                } else {
                    region.object().lock_page_tables()
                };
                let counted = region.stable.is_none();
                let obj_table = counted.then(|| pt.context_table_addr()).flatten();
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
                pt.remove_invalidate(arch.target, cursor);
                if counted && released {
                    pt.dec_map_count();
                    if pt.map_count() == 0 {
                        region.object().note_last_unmap();
                    }
                }
            }
        }
    }

    /// Init a context for being the kernel context, and clone the mappings from the bootstrap
    /// context.
    pub(super) fn init_kernel_context(&self) {
        let proto = unsafe { Mapper::current() };
        let rm = proto.readmap(MappingCursor::new(
            VirtAddr::start_kernel_memory(),
            usize::MAX,
        ));
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        for map in rm.coalesce() {
            let cursor = MappingCursor::new(map.vaddr(), map.len());
            let settings = MappingSettings::new(
                map.settings().perms(),
                map.settings().cache(),
                map.settings().flags() | MappingFlags::GLOBAL | MappingFlags::WIRED,
            );
            let mut phys = ContiguousProvider::new(map.paddr(), map.len(), settings);
            self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
        }

        // ID-map the lower memory. This is needed by some systems to boot secondary CPUs. This
        // mapping is cleared by the call to prep_smp later.
        let id_len = 0x100000000; // 4GB
        let cursor = MappingCursor::new(
            VirtAddr::new(
                Table::level_to_page_size(Table::last_level())
                    .try_into()
                    .unwrap(),
            )
            .unwrap(),
            id_len,
        );
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE | Protections::EXEC,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(
                Table::level_to_page_size(Table::last_level())
                    .try_into()
                    .unwrap(),
            )
            .unwrap(),
            id_len,
            settings,
        );

        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));

        let cursor = MappingCursor::new(VirtAddr::PHYS_START, PhysAddr::phys_mem_map_len());
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE | Protections::EXEC,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(0).unwrap(),
            PhysAddr::phys_mem_map_len(),
            settings,
        );

        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
    }

    pub fn lookup_slot(&self, slot: usize) -> Option<Arc<MapRegion>> {
        let slot = &Slot::try_from(slot).ok()?;
        self.regions
            .lock()
            .lookup_region(slot.start_vaddr())
            .cloned()
    }

    /// Fill `buf` with the numbers of the slots that have something mapped in them, ascending,
    /// skipping the first `offset`. Returns how many were written; short of `buf.len()` means the
    /// enumeration is done. Backs `sys_enumerate_slots`.
    pub fn enumerate_slots(&self, buf: &mut [u64], offset: usize) -> Result<usize, TwzError> {
        let mut slots = self
            .regions
            .lock()
            .mappings()
            .filter_map(|region| {
                Slot::try_from(region.range.start)
                    .ok()
                    .map(|s| s.raw() as u64)
            })
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();

        let count = slots.len().saturating_sub(offset).min(buf.len());
        buf[..count].copy_from_slice(&slots[offset..(offset + count)]);
        Ok(count)
    }
}

impl UserContext for VirtContext {
    type MappingInfo = Slot;
    type SwitchTarget = ArchContextTarget;

    fn switch_target(&self, sctx: ObjID) -> Option<ArchContextTarget> {
        self.target_cache.lock().get(&sctx).copied()
    }

    unsafe fn switch_to_target(&self, target: &ArchContextTarget) {
        let proc = tls_ready().then(current_processor);
        // Safety: the caller guarantees the target is still registered here.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    fn switch_to(&self, sctx: ObjID) {
        //let sctx = 0.into();
        let tc = self.target_cache.lock();
        let target = tc
            .get(&sctx)
            .expect("tried to switch to a non-registered sctx");
        // TLS/the processor registry isn't up yet during the very early boot switch from
        // memory::init(); pass None in that case rather than looking up current_processor()
        // from inside the arch-specific switch code.
        let proc = tls_ready().then(current_processor);
        // Safety: we get the target from an ArchContext that we track.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    fn insert_object(
        self: &Arc<Self>,
        slot: Slot,
        object_info: &ObjectContextInfo,
    ) -> Result<(), TwzError> {
        log::debug!(
            "insert {} to {:?} {:?}",
            object_info.object.id(),
            slot.start_vaddr(),
            object_info.prot(),
        );

        let mut stable = None;
        if object_info.flags.contains(MapFlags::STABLE) {
            stable = Some(Arc::new(Mutex::new(
                object_info.object().cow_clone_page_tables()?,
            )));
        }

        let (_is_ok, default_prot) = object_info.object.check_id();
        let new_slot_info = MapRegion {
            prot: object_info.prot(),
            cache_type: object_info.cache(),
            object: object_info.object().clone(),
            offset: 0,
            range: slot.range(),
            flags: object_info.flags,
            target_sctx: object_info.target_sctx(),
            stable,
            default_prot,
            should_sync: Arc::new(AtomicBool::new(false)),
            removed: Arc::new(AtomicBool::new(false)),
        };

        // Check the slot is free before mapping, and hold the lock across the map: otherwise a
        // racing insert can clobber our object table entry, and a Busy return leaves behind a
        // mapping plus the map count taken for it, which keeps the object from ever being reaped.
        // Lock order (regions -> object page tables -> secctx) matches insert_kernel_object.
        let mut slots = self.regions.lock();
        if slots.lookup_region(slot.start_vaddr()).is_some() {
            return Err(ResourceError::Busy.into());
        }
        self.map_object(&new_slot_info);
        slots.insert_region(new_slot_info);
        Ok(())
    }

    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object(info)
        } else {
            let mut slots = self.regions.lock();
            slots
                .lookup_region(info.start_vaddr())
                .map(|info| (&**info).into())
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut slots = self.regions.lock();
        let Some(slot) = slots.remove_region(info.start_vaddr()) else {
            return;
        };
        fault::note_unmap(info.raw(), slot.object());

        // Tear the mapping down while still holding the regions lock: insert_object claims a free
        // slot under it and maps immediately (see there), so releasing it here would let another
        // object be mapped into this slot and then have its entry removed by the unmap below.
        // Lock order (regions -> object page tables -> secctx) matches insert_object.
        {
            // Whichever page tables the fault path would use for this region -- taking the same
            // one is what makes the `removed` store below and that path's check of it ordered.
            let mut pt = if let Some(stable) = slot.stable.as_ref() {
                stable.lock()
            } else {
                slot.object().lock_page_tables()
            };
            // An in-flight fault now either mapped before us, and the unmap below undoes it, or
            // sees this and does not map at all. See MapRegion::handle_fault.
            slot.removed
                .store(true, core::sync::atomic::Ordering::SeqCst);

            // Stable regions map a private clone of the object's tables, and never took a count
            // against the object (see map_object), so there is nothing to give back for them.
            let counted = slot.stable.is_none();
            let obj_table = counted.then(|| pt.context_table_addr()).flatten();
            self.for_each_arch(|arch| {
                let cursor = slot.mapping_cursor(0, MAX_SIZE);
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
                if counted {
                    pt.remove_invalidate(arch.target, cursor);
                    if released {
                        pt.dec_map_count();
                        if pt.map_count() == 0 {
                            slot.object().note_last_unmap();
                        }
                    }
                }
            });
        }
        drop(slots);

        // After the unmap, not before: syncing can block on the pager, and dirty state lives in the
        // object's own page tables, which unmapping a context's reference to them does not touch.
        if slot.should_sync.load(core::sync::atomic::Ordering::SeqCst) {
            if let Err(e) = slot.ctrl(MapControlCmd::Sync(core::ptr::null_mut()), 0) {
                log::error!("failed to sync object {}: {:?}", slot.object().id(), e);
            }
        }
    }
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtContextSlot {
    obj: ObjectRef,
    slot: Slot,
    prot: Protections,
    cache: CacheType,
    flags: MapFlags,
}

impl From<&VirtContextSlot> for ObjectContextInfo {
    fn from(info: &VirtContextSlot) -> Self {
        ObjectContextInfo::new(info.obj.clone(), info.prot, info.cache, info.flags)
    }
}

impl Drop for VirtContext {
    fn drop(&mut self) {
        // TODO: remove appropriate invalidations from objects.
    }
}

pub const HEAP_MAX_LEN: usize = 0x0000001000000000 / 16; //4GB

struct GlobalPageAlloc {
    alloc: linked_list_allocator::Heap,
    end: VirtAddr,
}

impl GlobalPageAlloc {
    fn extend(&mut self, len: usize, mapper: &VirtContext) {
        let cursor = MappingCursor::new(self.end, len);
        // TODO: wait-ok?
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        // Uninit, not zeroed: nothing reads kernel heap memory before writing it. The
        // page-table frames `fa` supplies below are a different matter and stay zeroed.
        let mut phys = UninitPageProvider::new(FrameAllocFlags::KERNEL, settings);
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );

        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &mut fa);
        });
        self.end = self.end.offset(len).unwrap();
        // Safety: the extension is backed by memory that is directly after the previous call to
        // extend.
        unsafe {
            self.alloc.extend(len);
        }
    }

    fn init(&mut self, mapper: &VirtContext) {
        let len = 2 * 1024 * 1024;
        let cursor = MappingCursor::new(self.end, len);
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Uninit, not zeroed: nothing reads kernel heap memory before writing it. The
        // page-table frames `fa` supplies below are a different matter and stay zeroed.
        let mut phys = UninitPageProvider::new(FrameAllocFlags::KERNEL, settings);

        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &mut fa);
        });
        self.end = self.end.offset(len).unwrap();
        // Safety: the initial is backed by memory.
        unsafe {
            self.alloc.init(VirtAddr::HEAP_START.as_mut_ptr(), len);
        }
    }
}

// Safety: the internal heap contains raw pointers, which are not Send. However, the heap is
// globally mapped and static for the lifetime of the kernel.
unsafe impl Send for GlobalPageAlloc {}

static GLOBAL_PAGE_ALLOC: Spinlock<GlobalPageAlloc> = Spinlock::new(GlobalPageAlloc {
    alloc: linked_list_allocator::Heap::empty(),
    end: VirtAddr::HEAP_START,
});

impl KernelMemoryContext for VirtContext {
    fn allocate_chunk(&self, layout: core::alloc::Layout) -> Result<NonNull<u8>, TwzError> {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        let res = glb.alloc.allocate_first_fit(layout);
        match res {
            Err(_) => {
                let size = layout
                    .pad_to_align()
                    .size()
                    .next_multiple_of(Table::level_to_page_size(Table::last_level()))
                    * 2;
                glb.extend(size, self);
                glb.alloc
                    .allocate_first_fit(layout)
                    .map_err(|_| ResourceError::OutOfMemory.into())
            }
            Ok(x) => Ok(x),
        }
    }

    unsafe fn deallocate_chunk(&self, layout: core::alloc::Layout, ptr: NonNull<u8>) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        unsafe {
            glb.alloc.deallocate(ptr, layout);
        }
    }

    fn init_allocator(&self) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        glb.init(self);
    }

    fn prep_smp(&self) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        self.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap(
                MappingCursor::new(
                    VirtAddr::start_user_memory(),
                    VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
                ),
                &mut fa,
            )
        });
    }

    type Handle<T> = KernelObjectVirtHandle<T>;

    fn insert_kernel_object<T>(&self, info: ObjectContextInfo) -> Self::Handle<T> {
        let mut slots = self.regions.lock();
        let mut kernel_slots_counter = kernel_slot_counter().lock();
        let slot = kernel_slots_counter
            .kernel_slots_nums
            .pop()
            .unwrap_or_else(|| {
                let cur = kernel_slots_counter.cur_kernel_slot;
                kernel_slots_counter.cur_kernel_slot += 1;
                let max = Slot::try_from(
                    VirtAddr::end_kernel_object_memory()
                        .offset(-1isize)
                        .unwrap(),
                )
                .unwrap()
                .raw();
                if cur > max {
                    panic!("out of kernel object slots");
                }
                Slot(cur)
            });
        let (_is_ok, default_prot) = info.object().check_id();
        let new_slot_info = MapRegion {
            object: info.object().clone(),
            range: slot.range(),
            offset: 0,
            prot: info.prot(),
            cache_type: info.cache(),
            flags: info.flags,
            target_sctx: info.target_sctx(),
            stable: None,
            default_prot,
            should_sync: Arc::new(AtomicBool::new(false)),
            removed: Arc::new(AtomicBool::new(false)),
        };
        self.map_object(&new_slot_info);
        slots.insert_region(new_slot_info);
        KernelObjectVirtHandle {
            info,
            slot,
            _pd: PhantomData,
        }
    }
}

pub struct KernelObjectVirtHandle<T> {
    info: ObjectContextInfo,
    slot: Slot,
    _pd: PhantomData<T>,
}

impl<T> KernelObjectVirtHandle<T> {
    pub fn start_addr(&self) -> VirtAddr {
        VirtAddr::new(0)
            .unwrap()
            .offset(self.slot.raw() * MAX_SIZE)
            .unwrap()
    }

    pub fn id(&self) -> ObjID {
        self.info.object().id()
    }

    pub fn object(&self) -> &ObjectRef {
        self.info.object()
    }
}

impl<T> Drop for KernelObjectVirtHandle<T> {
    fn drop(&mut self) {
        let kctx = kernel_context();
        let region = {
            let mut slots = kctx.regions.lock();
            // We don't need to tell the object that it's no longer mapped in the kernel context,
            // since object invalidation always informs the kernel context.
            slots.remove_region(self.slot.start_vaddr())
        };
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut pt = self.object().lock_page_tables();
        // Under the page tables, as in VirtContext::remove_object: a fault holding a clone of this
        // region must not re-map it behind the unmap below.
        if let Some(region) = &region {
            region
                .removed
                .store(true, core::sync::atomic::Ordering::SeqCst);
        }
        let obj_table = pt.context_table_addr();
        let released = kctx.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap_object(
                MappingCursor::new(self.start_addr(), MAX_SIZE),
                obj_table,
                &mut fa,
            )
        });
        let last = released && {
            pt.dec_map_count();
            pt.map_count() == 0
        };
        drop(pt);
        if last {
            self.object().note_last_unmap();
        }
        kernel_slot_counter()
            .lock()
            .kernel_slots_nums
            .push(self.slot);
    }
}

impl<T> KernelObjectHandle<T> for KernelObjectVirtHandle<T> {
    fn base(&self) -> &T {
        unsafe {
            self.start_addr()
                .offset(NULLPAGE_SIZE)
                .unwrap()
                .as_ptr::<T>()
                .as_ref()
                .unwrap()
        }
    }

    fn base_mut(&mut self) -> &mut T {
        unsafe {
            self.start_addr()
                .offset(NULLPAGE_SIZE)
                .unwrap()
                .as_mut_ptr::<T>()
                .as_mut()
                .unwrap()
        }
    }

    fn lea_raw<R>(&self, iptr: *const R) -> Option<&R> {
        let offset = iptr as usize;
        let size = size_of::<R>();
        if offset >= MAX_SIZE || offset.checked_add(size)? >= MAX_SIZE {
            return None;
        }
        unsafe {
            Some(
                self.start_addr()
                    .offset(offset)
                    .unwrap()
                    .as_ptr::<R>()
                    .as_ref()
                    .unwrap(),
            )
        }
    }

    fn lea_raw_mut<R>(&self, iptr: *mut R) -> Option<&mut R> {
        let offset = iptr as usize;
        let size = size_of::<R>();
        if offset >= MAX_SIZE || offset.checked_add(size)? >= MAX_SIZE {
            return None;
        }
        unsafe {
            Some(
                self.start_addr()
                    .offset(offset)
                    .unwrap()
                    .as_mut_ptr::<R>()
                    .as_mut()
                    .unwrap(),
            )
        }
    }
}

impl StableId for VirtContext {
    fn id(&self) -> &Id<'_> {
        &self.id
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}
