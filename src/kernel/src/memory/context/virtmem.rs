//! This mod implements [UserContext] and [KernelMemoryContext] for virtual memory systems.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{marker::PhantomData, mem::size_of, ops::Range, ptr::NonNull};

use region::{MapRegion, RegionManager};
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_rt_abi::error::{ResourceError, TwzError};

use super::{
    kernel_context, KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, UserContext,
};
use crate::{
    arch::{
        address::VirtAddr,
        context::{ArchContext, ArchContextTarget},
    },
    idcounter::{Id, IdCounter, StableId},
    memory::{
        pagetables::{
            ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings,
            PhysAddrProvider, Table, ZeroPageProvider,
        },
        tracker::FrameAllocFlags,
        PhysAddr,
    },
    mutex::Mutex,
    obj::{self, pages::Page, ObjectRef, PageNumber},
    once::Once,
    security::KERNEL_SCTX,
    spinlock::Spinlock,
};

pub mod fault;
pub mod region;
mod tests;

pub use fault::page_fault;

/// A type that implements [Context] for virtual memory systems.
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

struct ObjectPageProvider<'a> {
    page: &'a Page,
}

impl<'a> PhysAddrProvider for ObjectPageProvider<'a> {
    fn peek(&mut self) -> Option<(crate::arch::address::PhysAddr, usize)> {
        Some((self.page.physical_address(), PageNumber::PAGE_SIZE))
    }

    fn consume(&mut self, _len: usize) {}
}

impl VirtContext {
    fn __new(is_kernel: bool) -> Self {
        Self {
            regions: Mutex::new(RegionManager::default()),
            is_kernel,
            id: CONTEXT_IDS.next(),
            secctx: Mutex::new(BTreeMap::new()),
            target_cache: Spinlock::new(BTreeMap::new()),
        }
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Self {
        let this = Self::__new(true);
        this.register_sctx(KERNEL_SCTX, ArchContext::new_kernel());
        this
    }

    /// Construct a new context for userspace.
    pub fn new() -> Self {
        let this = Self::__new(false);
        // TODO: remove this once we have full support for user security contexts
        this.register_sctx(KERNEL_SCTX, ArchContext::new());
        this
    }

    pub fn with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> R {
        let secctx = self.secctx.lock();
        cb(secctx
            .get(&sctx)
            .expect("cannot get arch mapper for unattached security context"))
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

    /// Init a context for being the kernel context, and clone the mappings from the bootstrap
    /// context.
    pub(super) fn init_kernel_context(&self) {
        let proto = unsafe { Mapper::current() };
        let rm = proto.readmap(MappingCursor::new(
            VirtAddr::start_kernel_memory(),
            usize::MAX,
        ));
        for map in rm.coalesce() {
            let cursor = MappingCursor::new(map.vaddr(), map.len());
            let mut phys = ContiguousProvider::new(map.paddr(), map.len());
            let settings = MappingSettings::new(
                map.settings().perms(),
                map.settings().cache(),
                map.settings().flags() | MappingFlags::GLOBAL,
            );
            self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &settings));
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
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(
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
            MappingFlags::empty(),
        );
        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &settings));
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
        let tc = self.target_cache.lock();
        let target = tc
            .get(&sctx)
            .expect("tried to switch to a non-registered sctx");
        // Safety: we get the target from an ArchContext that we track.
        unsafe {
            ArchContext::switch_to_target(target);
        }
    }

    fn insert_object(
        self: &Arc<Self>,
        slot: Slot,
        object_info: &ObjectContextInfo,
    ) -> Result<(), TwzError> {
        let new_slot_info = MapRegion {
            prot: object_info.prot(),
            cache_type: object_info.cache(),
            object: object_info.object().clone(),
            offset: 0,
            range: slot.range(),
        };
        object_info.object().add_context(self);
        let mut slots = self.regions.lock();
        if let Some(info) = slots.lookup_region(slot.start_vaddr()) {
            if info != &new_slot_info {
                return Err(ResourceError::Busy.into());
            }
            return Ok(());
        }
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

    fn invalidate_object(
        &self,
        obj: ObjID,
        range: &core::ops::Range<PageNumber>,
        mode: obj::InvalidateMode,
    ) {
        let start = range.start.as_byte_offset();
        let len = range.end.as_byte_offset() - start;
        let mut slots = self.regions.lock();
        let arches = self.secctx.lock();
        for arch in arches.values() {
            for info in slots.object_mappings(obj) {
                match mode {
                    obj::InvalidateMode::Full => {
                        arch.unmap(info.mapping_cursor(start, len));
                    }
                    obj::InvalidateMode::WriteProtect => {
                        arch.change(
                            info.mapping_cursor(start, len),
                            &info.mapping_settings(true, self.is_kernel),
                        );
                    }
                }
            }
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        let mut slots = self.regions.lock();
        if let Some(slot) = slots.remove_region(info.start_vaddr()) {
            let arches = self.secctx.lock();
            for arch in arches.values() {
                arch.unmap(slot.mapping_cursor(0, MAX_SIZE));
            }
            slot.object.remove_context(self.id.value());
        }
    }
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtContextSlot {
    obj: ObjectRef,
    slot: Slot,
    prot: Protections,
    cache: CacheType,
}

impl From<&VirtContextSlot> for ObjectContextInfo {
    fn from(info: &VirtContextSlot) -> Self {
        ObjectContextInfo::new(info.obj.clone(), info.prot, info.cache)
    }
}

impl Drop for VirtContext {
    fn drop(&mut self) {
        let id = self.id().value();
        // cleanup and object's context info
        for info in self.regions.get_mut().mappings() {
            info.object.remove_context(id)
        }
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
        let mut phys = ZeroPageProvider::new(FrameAllocFlags::KERNEL);
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &settings);
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
        let mut phys = ZeroPageProvider::new(FrameAllocFlags::KERNEL);
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &settings);
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
    fn allocate_chunk(&self, layout: core::alloc::Layout) -> NonNull<u8> {
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
                glb.alloc.allocate_first_fit(layout).unwrap()
            }
            Ok(x) => x,
        }
    }

    unsafe fn deallocate_chunk(&self, layout: core::alloc::Layout, ptr: NonNull<u8>) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        glb.alloc.deallocate(ptr, layout);
    }

    fn init_allocator(&self) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        glb.init(self);
    }

    fn prep_smp(&self) {
        self.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap(MappingCursor::new(
                VirtAddr::start_user_memory(),
                VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
            ))
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
        };
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

impl<T> Clone for KernelObjectVirtHandle<T> {
    fn clone(&self) -> Self {
        Self {
            info: self.info.clone(),
            slot: self.slot,
            _pd: PhantomData,
        }
    }
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
        kctx.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap(MappingCursor::new(self.start_addr(), MAX_SIZE));
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
    #[derive(Debug)]
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}
