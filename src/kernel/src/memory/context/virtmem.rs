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
            PhysAddrProvider, PhysMapInfo, Table, ZeroPageProvider,
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
    fn __new(is_kernel: bool) -> Self {
        let mut secctx = Mutex::new(BTreeMap::new());
        // We ensure that the BTree never changes while we hold the lock.
        secctx.set_safe_with_spinlocks(true);
        let new = Self {
            regions: Mutex::new(RegionManager::default()),
            is_kernel,
            id: CONTEXT_IDS.next(),
            secctx,
            target_cache: Spinlock::new(BTreeMap::new()),
        };
        new
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Arc<Self> {
        let this = Arc::new(Self::__new(true));
        this.register_sctx(KERNEL_SCTX, ArchContext::new_kernel());
        // Cache the root now, while we're safely outside the thread-switch path.
        KERNEL_ARCH_TARGET.call_once(|| {
            *this
                .target_cache
                .lock()
                .get(&KERNEL_SCTX)
                .expect("kernel sctx just registered")
        });
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
        let this = Arc::new(Self::__new(false));
        // TODO: remove this once we have full support for user security contexts
        this.register_sctx(KERNEL_SCTX, ArchContext::new());
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), this.clone());
        this
    }

    pub fn try_with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> Option<R> {
        let secctx = self.secctx.lock();
        secctx.get(&sctx).map(|arch| cb(arch))
    }

    pub fn with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> R {
        //let sctx = 0.into();
        let secctx = self.secctx.lock();
        cb(secctx
            .get(&sctx)
            .expect("cannot get arch mapper for unattached security context"))
    }

    pub fn map_object(&self, info: &MapRegion, default_prots: Protections) {
        let sctx = current_thread_ref()
            .map(|ct| ct.secctx.active_id())
            .unwrap_or(KERNEL_SCTX);

        let len = info.range.end - info.range.start;
        let cursor = MappingCursor::new(info.range.start, len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
            FrameAllocFlags::WAIT_OK,
        );
        if let Ok(sctx) = crate::security::get_sctx(sctx) {
            let perms = sctx.lookup(info.object().id(), default_prots);
            let mut pt = if info.stable.is_some() {
                info.stable.as_ref().unwrap().lock()
            } else {
                info.object.lock_page_tables()
            };
            self.try_with_arch(sctx.id(), |arch| {
                pt.add_invalidate(arch.target.paddr(), cursor);
                let settings = MappingSettings::new(
                    perms.effective(default_prots, info.prot),
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
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
            FrameAllocFlags::WAIT_OK,
        );
        self.with_arch(sctxid, |arch| {
            object_tables.add_invalidate(arch.target.paddr(), cursor);
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

        drop(secctx);

        if let Some(arch) = arch {
            let regions = self.regions.lock();
            let capacity = regions.mappings().count();
            drop(regions);

            let mut region_list = alloc::vec::Vec::with_capacity(capacity);
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
                let did_unmap = arch.unmap(cursor, &mut fa);
                if region.stable.is_none() {
                    let mut pt = region.object().lock_page_tables();
                    pt.remove_invalidate(arch.target.paddr(), cursor);
                    if did_unmap {
                        pt.dec_map_count();
                        if pt.map_count() == 0 {
                            region.object().note_last_unmap();
                        }
                    }
                }
            }
        }

        secctx = self.secctx.lock();
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

    pub fn lookup_slot(&self, slot: usize) -> Option<MapRegion> {
        let slot = &Slot::try_from(slot).ok()?;
        self.regions
            .lock()
            .lookup_region(slot.start_vaddr())
            .cloned()
    }
}

impl UserContext for VirtContext {
    type MappingInfo = Slot;

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

        let new_slot_info = MapRegion {
            prot: object_info.prot(),
            cache_type: object_info.cache(),
            object: object_info.object().clone(),
            offset: 0,
            range: slot.range(),
            flags: object_info.flags,
            stable,
            should_sync: Arc::new(AtomicBool::new(false)),
        };

        let (_is_ok, default_prots) = object_info.object.check_id();

        // Check the slot is free before mapping, and hold the lock across the map: otherwise a
        // racing insert can clobber our object table entry, and a Busy return leaves behind a
        // mapping plus the map count taken for it, which keeps the object from ever being reaped.
        // Lock order (regions -> object page tables -> secctx) matches insert_kernel_object.
        let mut slots = self.regions.lock();
        if slots.lookup_region(slot.start_vaddr()).is_some() {
            return Err(ResourceError::Busy.into());
        }
        self.map_object(&new_slot_info, default_prots);
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
                .map(|info| info.into())
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut slots = self.regions.lock();
        if let Some(slot) = slots.remove_region(info.start_vaddr()) {
            drop(slots);
            fault::note_unmap(info.raw(), slot.object());
            if slot.should_sync.load(core::sync::atomic::Ordering::SeqCst) {
                if let Err(e) = slot.ctrl(MapControlCmd::Sync(core::ptr::null_mut()), 0) {
                    log::error!("failed to sync object {}: {:?}", slot.object().id(), e);
                }
            }
            let mut pt = slot
                .stable
                .is_none()
                .then(|| slot.object().lock_page_tables());
            let arches = self.secctx.lock();
            for arch in arches.values() {
                let cursor = slot.mapping_cursor(0, MAX_SIZE);
                let did_unmap = arch.unmap(cursor, &mut fa);
                if let Some(pt) = pt.as_mut() {
                    pt.remove_invalidate(arch.target.paddr(), cursor);
                    if did_unmap {
                        pt.dec_map_count();
                        if pt.map_count() == 0 {
                            slot.object().note_last_unmap();
                        }
                    }
                }
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
        let mut phys = ZeroPageProvider::new(FrameAllocFlags::KERNEL, settings);
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
        let mut phys = ZeroPageProvider::new(FrameAllocFlags::KERNEL, settings);

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
        let new_slot_info = MapRegion {
            object: info.object().clone(),
            range: slot.range(),
            offset: 0,
            prot: info.prot(),
            cache_type: info.cache(),
            flags: info.flags,
            stable: None,
            should_sync: Arc::new(AtomicBool::new(false)),
        };
        let (_is_ok, default_prots) = info.object().check_id();
        self.map_object(&new_slot_info, default_prots);
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
        {
            let mut slots = kctx.regions.lock();
            // We don't need to tell the object that it's no longer mapped in the kernel context,
            // since object invalidation always informs the kernel context.
            slots.remove_region(self.slot.start_vaddr());
        }
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        kctx.with_arch(KERNEL_SCTX, |arch| {
            if arch.unmap(MappingCursor::new(self.start_addr(), MAX_SIZE), &mut fa) {
                let mut pt = self.object().lock_page_tables();
                pt.dec_map_count();
                let last = pt.map_count() == 0;
                drop(pt);
                if last {
                    self.object().note_last_unmap();
                }
            }
        });
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
