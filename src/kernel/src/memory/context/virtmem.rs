//! This mod implements [UserContext] and [KernelMemoryContext] for virtual memory systems.

use core::{intrinsics::size_of, marker::PhantomData, ptr::NonNull};

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryError, ObjectMemoryFaultInfo,
        UpcallInfo,
    },
};

use super::{
    kernel_context, InsertError, KernelMemoryContext, KernelObjectHandle, ObjectContextInfo,
    UserContext,
};
use crate::{
    arch::{address::VirtAddr, context::ArchContext},
    idcounter::{Id, IdCounter, StableId},
    memory::{
        pagetables::{
            ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings,
            PhysAddrProvider, Table, ZeroPageProvider,
        },
        PhysAddr,
    },
    mutex::Mutex,
    obj::{self, ObjectRef},
    spinlock::Spinlock,
    thread::current_thread_ref,
};

use crate::{
    obj::{pages::Page, PageNumber},
    thread::current_memory_context,
};

/// A type that implements [Context] for virtual memory systems.
pub struct VirtContext {
    arch: ArchContext,
    sctx: Option<ObjectRef>,
    slots: Mutex<SlotMgr>,
    id: Id<'static>,
    is_kernel: bool,
}

static CONTEXT_IDS: IdCounter = IdCounter::new();

struct KernelSlotCounter {
    cur_kernel_slot: usize,
    kernel_slots_nums: Vec<Slot>,
}

#[derive(Default)]
struct SlotMgr {
    slots: BTreeMap<Slot, VirtContextSlot>,
    objs: BTreeMap<ObjID, Vec<Slot>>,
}

lazy_static::lazy_static! {
    static ref KERNEL_SLOT_COUNTER: Mutex<KernelSlotCounter> = Mutex::new(KernelSlotCounter {
        cur_kernel_slot: Slot::try_from(VirtAddr::start_kernel_object_memory()).unwrap().raw(),
        kernel_slots_nums: Vec::new(),
    });
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

impl SlotMgr {
    fn get(&self, slot: &Slot) -> Option<&VirtContextSlot> {
        self.slots.get(slot)
    }

    fn insert(&mut self, slot: Slot, id: ObjID, info: VirtContextSlot) {
        self.slots.insert(slot, info);
        let list = self.objs.entry(id).or_default();
        list.push(slot);
    }

    fn remove(&mut self, slot: Slot) -> Option<VirtContextSlot> {
        if let Some(info) = self.slots.remove(&slot) {
            let v = self.objs.get_mut(&info.obj.id()).unwrap();
            let pos = v.iter().position(|item| *item == slot).unwrap();
            v.remove(pos);
            Some(info)
        } else {
            None
        }
    }

    fn obj_to_slots(&self, id: ObjID) -> Option<&[Slot]> {
        self.objs.get(&id).map(|x| x.as_slice())
    }
}

struct ObjectPageProvider<'a> {
    page: &'a Page,
}

impl<'a> PhysAddrProvider for ObjectPageProvider<'a> {
    fn peek(&mut self) -> (crate::arch::address::PhysAddr, usize) {
        (self.page.physical_address(), PageNumber::PAGE_SIZE)
    }

    fn consume(&mut self, _len: usize) {}
}

impl Default for VirtContext {
    fn default() -> Self {
        Self::new(None)
    }
}

impl VirtContext {
    fn __new(arch: ArchContext, is_kernel: bool, sctx: Option<ObjectRef>) -> Self {
        Self {
            arch,
            slots: Mutex::new(SlotMgr::default()),
            is_kernel,
            id: CONTEXT_IDS.next(),
            sctx,
        }
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Self {
        Self::__new(ArchContext::new_kernel(), true, None)
    }

    /// Construct a new context for userspace.
    pub fn new(sctx: Option<ObjectRef>) -> Self {
        Self::__new(ArchContext::new(), false, sctx)
    }

    /// Init a context for being the kernel context, and clone the mappings from the bootstrap context.
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
            self.arch.map(cursor, &mut phys, &settings);
        }

        // ID-map the lower memory. This is needed by some systems to boot secondary CPUs. This mapping is cleared by
        // the call to prep_smp later.
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
        self.arch.map(cursor, &mut phys, &settings);
    }

    pub fn lookup_slot(&self, slot: usize) -> Option<VirtContextSlot> {
        self.slots.lock().get(&Slot::try_from(slot).ok()?).cloned()
    }
}

impl UserContext for VirtContext {
    type MappingInfo = Slot;

    fn switch_to(&self) {
        self.arch.switch_to();
    }

    fn insert_object(
        self: &Arc<Self>,
        slot: Slot,
        object_info: &ObjectContextInfo,
    ) -> Result<(), InsertError> {
        let new_slot_info = VirtContextSlot {
            obj: object_info.object().clone(),
            slot,
            prot: object_info.prot(),
            cache: object_info.cache(),
        };
        object_info.object().add_context(self);
        let mut slots = self.slots.lock();
        if let Some(info) = slots.get(&slot) {
            if info != &new_slot_info {
                return Err(InsertError::Occupied);
            }
            return Ok(());
        }
        slots.insert(slot, object_info.object().id(), new_slot_info);
        Ok(())
    }

    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object(info)
        } else {
            let slots = self.slots.lock();
            slots.get(&info).map(|info| info.into())
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
        let slots = self.slots.lock();
        if let Some(maps) = slots.obj_to_slots(obj) {
            for map in maps {
                let info = slots
                    .get(map)
                    .expect("invalid slot info for a mapped object");
                match mode {
                    obj::InvalidateMode::Full => {
                        self.arch.unmap(info.mapping_cursor(start, len));
                    }
                    obj::InvalidateMode::WriteProtect => {
                        self.arch.change(
                            info.mapping_cursor(start, len),
                            &info.mapping_settings(true, self.is_kernel),
                        );
                    }
                }
            }
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        let mut slots = self.slots.lock();
        if let Some(slot) = slots.remove(info) {
            self.arch.unmap(slot.mapping_cursor(0, MAX_SIZE));
            slot.obj.remove_context(self.id.value());
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

impl VirtContextSlot {
    fn mapping_cursor(&self, start: usize, len: usize) -> MappingCursor {
        MappingCursor::new(self.slot.start_vaddr().offset(start).unwrap(), len)
    }

    pub fn mapping_settings(&self, wp: bool, is_kern_obj: bool) -> MappingSettings {
        let mut prot = self.prot;
        if wp {
            prot.remove(Protections::WRITE);
        }
        MappingSettings::new(
            prot,
            self.cache,
            if is_kern_obj {
                MappingFlags::GLOBAL
            } else {
                MappingFlags::USER
            },
        )
    }

    pub fn object(&self) -> &ObjectRef {
        &self.obj
    }

    fn phys_provider<'a>(&self, page: &'a Page) -> ObjectPageProvider<'a> {
        ObjectPageProvider { page }
    }
}

impl Drop for VirtContext {
    fn drop(&mut self) {
        let id = self.id().value();
        // cleanup and object's context info
        for info in self.slots.get_mut().slots.values() {
            info.obj.remove_context(id)
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
        let mut phys = ZeroPageProvider::default();
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        mapper.arch.map(cursor, &mut phys, &settings);
        self.end = self.end.offset(len).unwrap();
        // Safety: the extension is backed by memory that is directly after the previous call to extend.
        unsafe {
            self.alloc.extend(len);
        }
    }

    fn init(&mut self, mapper: &VirtContext) {
        let len = 2 * 1024 * 1024;
        let cursor = MappingCursor::new(self.end, len);
        let mut phys = ZeroPageProvider::default();
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        mapper.arch.map(cursor, &mut phys, &settings);
        self.end = self.end.offset(len).unwrap();
        // Safety: the initial is backed by memory.
        unsafe {
            self.alloc.init(VirtAddr::HEAP_START.as_mut_ptr(), len);
        }
    }
}

// Safety: the internal heap contains raw pointers, which are not Send. However, the heap is globally mapped and static
// for the lifetime of the kernel.
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
        self.arch.unmap(MappingCursor::new(
            VirtAddr::start_user_memory(),
            VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
        ));
    }

    type Handle<T> = KernelObjectVirtHandle<T>;

    fn insert_kernel_object<T>(&self, info: ObjectContextInfo) -> Self::Handle<T> {
        let mut slots = self.slots.lock();
        let mut kernel_slots_counter = KERNEL_SLOT_COUNTER.lock();
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
        let new_slot_info = VirtContextSlot {
            obj: info.object().clone(),
            slot,
            prot: info.prot(),
            cache: info.cache(),
        };
        slots.insert(slot, info.object().id(), new_slot_info);
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
            let mut slots = kctx.slots.lock();
            // We don't need to tell the object that it's no longer mapped in the kernel context, since object
            // invalidation always informs the kernel context.
            slots.remove(self.slot);
        }
        kctx.arch
            .unmap(MappingCursor::new(self.start_addr(), MAX_SIZE));
        KERNEL_SLOT_COUNTER.lock().kernel_slots_nums.push(self.slot);
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

pub fn page_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    //logln!("page-fault: {:?} {:?} {:?} ip={:?}", addr, cause, flags, ip);
    if flags.contains(PageFaultFlags::INVALID) {
        panic!("page table contains invalid bits for address {:?}", addr);
    }
    if !flags.contains(PageFaultFlags::USER) && addr.is_kernel() && !addr.is_kernel_object_memory()
    {
        panic!(
            "kernel page-fault at IP {:?} caused by {:?} to/from {:?} with flags {:?}",
            ip, cause, addr, flags
        );
    } else {
        if flags.contains(PageFaultFlags::USER) && addr.is_kernel() {
            current_thread_ref()
                .unwrap()
                .send_upcall(UpcallInfo::MemoryContextViolation(
                    MemoryContextViolationInfo::new(addr.raw(), cause),
                ));
            return;
        }

        let user_ctx = current_memory_context();
        let (ctx, is_kern_obj) = if addr.is_kernel_object_memory() {
            assert!(!flags.contains(PageFaultFlags::USER));
            (kernel_context(), true)
        } else {
            (user_ctx.as_ref().unwrap_or_else(||
            panic!("page fault in userland with no memory context at IP {:?} caused by {:?} to/from {:?} with flags {:?}, thread {}", ip, cause, addr, flags, current_thread_ref().map_or(0, |t| t.id()))), false)
        };
        let slot = match addr.try_into() {
            Ok(s) => s,
            Err(_) => {
                current_thread_ref()
                    .unwrap()
                    .send_upcall(UpcallInfo::MemoryContextViolation(
                        MemoryContextViolationInfo::new(addr.raw(), cause),
                    ));
                return;
            }
        };

        let slot_mgr = ctx.slots.lock();

        if let Some(info) = slot_mgr.get(&slot) {
            let page_number = PageNumber::from_address(addr);
            let mut obj_page_tree = info.obj.lock_page_tree();
            if page_number.is_zero() {
                current_thread_ref()
                    .unwrap()
                    .send_upcall(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
                        info.obj.id(),
                        ObjectMemoryError::NullPageAccess,
                        cause,
                        addr.into(),
                    )));
                return;
            }
            if page_number.as_byte_offset() >= MAX_SIZE {
                current_thread_ref()
                    .unwrap()
                    .send_upcall(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
                        info.obj.id(),
                        ObjectMemoryError::OutOfBounds(page_number.as_byte_offset()),
                        cause,
                        addr.into(),
                    )));
                return;
            }

            if let Some((page, cow)) =
                obj_page_tree.get_page(page_number, cause == MemoryAccessKind::Write)
            {
                ctx.arch.map(
                    info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                    &mut info.phys_provider(&page),
                    &info.mapping_settings(cow, is_kern_obj),
                );
                ctx.arch.change(
                    info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                    &info.mapping_settings(cow, is_kern_obj),
                );
            } else {
                let page = Page::new();
                obj_page_tree.add_page(page_number, page);
                let (page, cow) = obj_page_tree
                    .get_page(page_number, cause == MemoryAccessKind::Write)
                    .unwrap();
                ctx.arch.map(
                    info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                    &mut info.phys_provider(&page),
                    &info.mapping_settings(cow, is_kern_obj),
                );
                ctx.arch.change(
                    info.mapping_cursor(page_number.as_byte_offset(), PageNumber::PAGE_SIZE),
                    &info.mapping_settings(cow, is_kern_obj),
                );
            }
        } else {
            current_thread_ref()
                .unwrap()
                .send_upcall(UpcallInfo::MemoryContextViolation(
                    MemoryContextViolationInfo::new(addr.raw(), cause),
                ));
        }
    }
}

#[cfg(test)]
mod test {
    use alloc::sync::Arc;
    use twizzler_abi::{marker::BaseType, object::Protections};
    use twizzler_kernel_macros::kernel_test;

    use crate::memory::context::{
        kernel_context, KernelMemoryContext, KernelObjectHandle, ObjectContextInfo,
    };

    struct Foo {
        x: u32,
    }

    impl BaseType for Foo {
        fn init<T>(_t: T) -> Self {
            Foo { x: 0 }
        }

        fn tags() -> &'static [(
            twizzler_abi::marker::BaseVersion,
            twizzler_abi::marker::BaseTag,
        )] {
            todo!()
        }
    }

    #[kernel_test]
    fn test_kernel_object() {
        let obj = crate::obj::Object::new();
        let obj = Arc::new(obj);
        crate::obj::register_object(obj.clone());

        let ctx = kernel_context();
        let mut handle = ctx.insert_kernel_object(ObjectContextInfo::new(
            obj,
            Protections::READ | Protections::WRITE,
            twizzler_abi::device::CacheType::WriteBack,
        ));

        *handle.base_mut() = Foo { x: 42 };
    }
}
