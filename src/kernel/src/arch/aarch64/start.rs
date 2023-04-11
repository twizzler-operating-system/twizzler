use alloc::vec::Vec;

use limine::*;

use crate::{
    initrd::BootModule,
    memory::{MemoryRegion, MemoryRegionKind, VirtAddr, PhysAddr},
    BootInfo,
};

pub enum BootInfoSystemTable {
    Unknown
}

/// Bootstrap information passed in by the bootloader.
struct Armv8BootInfo {
    /// The memory map available to the processor.
    ///
    /// It is okay to use Vec here since the memory
    /// allocator initially uses some reserved stack memory.
    memory: Vec<MemoryRegion>,
}

impl BootInfo for Armv8BootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion] {
        unsafe { core::intrinsics::transmute(&self.memory[..]) }
    }

    fn get_modules(&self) -> &'static [BootModule] {
        todo!("get modules")
    }

    fn kernel_image_info(&self) -> (VirtAddr, usize) {
        todo!("kernel image info")
    }

    fn get_system_table(&self, _table: BootInfoSystemTable) -> VirtAddr {
        todo!("get system table")
    }

    fn get_cmd_line(&self) -> &'static str {
        // TODO
        ""
    }
}

impl From<LimineMemoryMapEntryType> for MemoryRegionKind {
    fn from(st: LimineMemoryMapEntryType) -> Self {
        match st {
            LimineMemoryMapEntryType::Usable => MemoryRegionKind::UsableRam,
            LimineMemoryMapEntryType::KernelAndModules => MemoryRegionKind::BootloaderReserved,
            _ => MemoryRegionKind::Reserved,
        }
    }
}

#[used]
static ENTRY_POINT: LimineEntryPointRequest = LimineEntryPointRequest::new(0)
    .entry(LiminePtr::new(limine_entry));

#[used]
static MEMORY_MAP: LimineMmapRequest = LimineMmapRequest::new(0);

#[link_section = ".limine_reqs"]
#[used]
static LR1: &'static LimineEntryPointRequest = &ENTRY_POINT;

#[link_section = ".limine_reqs"]
#[used]
static LR2: &'static LimineMmapRequest = &MEMORY_MAP;

// the kernel's entry point function from the limine bootloader
// limine ensures we are in el1 (kernel mode)
fn limine_entry() -> ! {
    emerglogln!("[kernel] hello world!!");

    // let's see what's in the memory map from limine
    let mmap = MEMORY_MAP
        .get_response() // LiminePtr<LimineMemmapResponse>
        .get() // Option<'static T>
        .expect("no memory map specified for kernel") // LimineMemmapResponse
        .mmap() // Option<&'static [LimineMemmapEntry]>
        .unwrap(); // &'static [LimineMemmapEntry]

    // emerglogln!("[kernel] printing out memory map");

    // for region in mmap {
    //     emerglogln!("\tfound: {:#018x} - {:#018x} ({} KB) {:?}",
    //         region.base,
    //         region.base + region.len,
    //         region.len >> 10,
    //         region.typ)
    // }

    // generate generic boot info
    let mut boot_info = Armv8BootInfo {
        memory: alloc::vec![],
    };

    // convert memory map from bootloader to memory regions
    boot_info.memory = mmap
        .iter()
        .map(|m| MemoryRegion {
            kind: m.typ.into(),
            start: PhysAddr::new(m.base).unwrap(),
            length: m.len as usize,
        })
        .collect();

    crate::kernel_main(&mut boot_info)
}
