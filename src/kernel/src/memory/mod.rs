use alloc::{boxed::Box, sync::Arc};
use x86_64::{
    structures::paging::{FrameAllocator, Size4KiB},
    PhysAddr, VirtAddr,
};

use crate::{arch, mutex::Mutex, BootInfo};

pub mod allocator;
pub mod context;
pub mod fault;
pub mod frame;
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MemoryRegionKind {
    UsableRam,
    Reserved,
    BootloaderReserved,
}
pub struct MemoryRegion {
    pub start: PhysAddr,
    pub length: usize,
    pub kind: MemoryRegionKind,
}
#[derive(Debug)]
pub enum MapFailed {
    FrameAllocation,
}

pub struct MappingIter<'a> {
    ctx: &'a MemoryContext,
    next: VirtAddr,
    done: bool,
}

impl<'a> MappingIter<'a> {
    fn new(ctx: &'a MemoryContext, start: VirtAddr) -> Self {
        Self {
            ctx,
            next: start,
            done: false,
        }
    }
}

use self::{
    context::{MapFlags, MemoryContext, MemoryContextRef},
    frame::{alloc_frame, PhysicalFrameFlags},
};
#[derive(Clone, Copy, Debug)]
pub struct MappingInfo {
    pub addr: VirtAddr,
    pub frame: PhysAddr,
    pub length: usize,
    pub flags: MapFlags,
}

impl MappingInfo {
    pub fn new(addr: VirtAddr, frame: PhysAddr, length: usize, flags: MapFlags) -> Self {
        Self {
            addr,
            frame,
            length,
            flags,
        }
    }
}

impl<'a> Iterator for MappingIter<'a> {
    type Item = MappingInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let info = self.ctx.arch.get_map(self.next);
        if let Some(info) = info {
            if self.next.as_u64().checked_add(info.length as u64).is_none() {
                self.done = true;
            } else {
                self.next += info.length;
            }
        }
        info
    }
}

fn init_kernel_context(clone_regions: &[VirtAddr]) -> MemoryContext {
    let ctx = MemoryContext::current();
    let mut new_context = MemoryContext::new_blank();

    let phys_mem_offset = arch::memory::phys_to_virt(PhysAddr::new(0));
    /* TODO: map ALL of the physical address space */
    new_context
        .arch
        .map(
            phys_mem_offset,
            PhysAddr::new(0),
            0x100000000,
            MapFlags::READ | MapFlags::WRITE | MapFlags::GLOBAL,
        )
        .unwrap();

    new_context
        .arch
        .map(
            VirtAddr::new(0),
            PhysAddr::new(0),
            0x100000000,
            MapFlags::READ | MapFlags::WRITE | MapFlags::GLOBAL | MapFlags::EXECUTE,
        )
        .unwrap();

    for va in clone_regions {
        new_context.clone_region(&ctx, *va);
    }
    unsafe {
        new_context.arch.switch();
    }
    new_context
}

struct KernelMemoryManagerInner {
    kernel_context: MemoryContext,
}
pub struct KernelMemoryManager {
    inner: spin::Mutex<KernelMemoryManagerInner>,
}

impl KernelMemoryManager {
    pub fn map_zero_pages(&self, addr: VirtAddr, length: usize) -> Result<(), ()> {
        let mut innerm = self.inner.lock();
        let inner = &mut *innerm;

        let mut count = 0;
        /* TODO: we could make this better, probably, by hooking more directly into arch-dep to allow it to map larger regions more automatically. */
        loop {
            let frame = alloc_frame(PhysicalFrameFlags::ZEROED);
            let va = arch::memory::phys_to_virt(frame.start_address());
            unsafe {
                let p: *mut u8 = va.as_mut_ptr();
                p.write_bytes(0, 0x1000);
            }
            let _res = inner.kernel_context.arch.map(
                addr + count,
                frame.start_address(),
                frame.size() as usize,
                MapFlags::READ | MapFlags::WRITE | MapFlags::GLOBAL | MapFlags::WIRED,
            );
            count += frame.size();
            if count >= length {
                break;
            }
        }

        Ok(())
    }

    pub fn premap(&self, start: VirtAddr, length: usize, page_size: usize) {
        self.inner
            .lock()
            .kernel_context
            .arch
            .premap(
                start,
                length,
                page_size,
                MapFlags::READ | MapFlags::WRITE | MapFlags::GLOBAL | MapFlags::WIRED,
            )
            .unwrap();
    }
}

static mut KERNEL_MEMORY_MANAGER: *mut KernelMemoryManager = core::ptr::null_mut();

pub fn kernel_memory_manager() -> &'static KernelMemoryManager {
    unsafe {
        KERNEL_MEMORY_MANAGER
            .as_ref()
            .expect("tried to get reference to kernel memory manager before it was setup")
    }
}

pub fn finish_setup() {
    let kc = &mut *kernel_memory_manager().inner.lock();
    kc.kernel_context
        .arch
        .unmap(VirtAddr::new(0), 0x100000000 /*TODO */);
    unsafe {
        let cr3 = x86::controlregs::cr3();
        x86::controlregs::cr3_write(cr3);
    }
}

pub fn init<B: BootInfo>(boot_info: &B, clone_regions: &[VirtAddr]) {
    frame::init(boot_info.memory_regions());
    let kernel_context = init_kernel_context(clone_regions);

    unsafe {
        KERNEL_MEMORY_MANAGER = Box::into_raw(Box::new(KernelMemoryManager {
            inner: spin::Mutex::new(KernelMemoryManagerInner {
                kernel_context: kernel_context,
            }),
        }))
    };

    allocator::init(kernel_memory_manager());
}
