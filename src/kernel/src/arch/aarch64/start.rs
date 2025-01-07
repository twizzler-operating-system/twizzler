use alloc::vec::Vec;
use core::ops::RangeInclusive;

use limine::{request::*, *};

use crate::{
    initrd::BootModule,
    memory::{MemoryRegion, MemoryRegionKind, PhysAddr, VirtAddr},
    BootInfo,
};

pub enum BootInfoSystemTable {
    Dtb,
    Efi,
}

/// Bootstrap information passed in by the bootloader.
struct Armv8BootInfo {
    /// The memory map available to the processor.
    ///
    /// It is okay to use Vec here since the memory
    /// allocator initially uses some reserved stack memory.
    memory: Vec<MemoryRegion>,

    /// A reference to the kernel's ELF file in memory.
    ///
    /// This contains other useful information such as the kernel's
    /// command line parameters.
    kernel: &'static limine::file::File,

    /// A list of user programs loaded into memory.
    ///
    /// This is essentially our initial ramdisk used
    /// to start userspace.
    modules: Vec<BootModule>,
}

impl BootInfo for Armv8BootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion] {
        unsafe { core::intrinsics::transmute(&self.memory[..]) }
    }

    fn get_modules(&self) -> &'static [BootModule] {
        unsafe { core::intrinsics::transmute(&self.modules[..]) }
    }

    fn kernel_image_info(&self) -> (VirtAddr, usize) {
        (
            VirtAddr::from_ptr(self.kernel.addr()),
            self.kernel.size() as usize,
        )
    }

    fn get_system_table(&self, table: BootInfoSystemTable) -> VirtAddr {
        match table {
            BootInfoSystemTable::Dtb => match DTB_REQ.get_response() {
                Some(resp) => VirtAddr::from_ptr(resp.dtb_ptr()),
                None => VirtAddr::new(0).unwrap(),
            },
            BootInfoSystemTable::Efi => todo!("get EFI system table"),
        }
    }

    fn get_cmd_line(&self) -> &'static str {
        if !self.kernel.cmdline().is_empty() {
            core::str::from_utf8(self.kernel.cmdline()).unwrap()
        } else {
            ""
        }
    }
}

use limine::memory_map::EntryType;
impl From<EntryType> for MemoryRegionKind {
    fn from(st: EntryType) -> Self {
        match st {
            EntryType::USABLE => MemoryRegionKind::UsableRam,
            EntryType::KERNEL_AND_MODULES => MemoryRegionKind::BootloaderReserved,
            _ => MemoryRegionKind::Reserved,
        }
    }
}

#[used]
#[link_section = ".limine_reqs"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".limine_reqs"]
static ENTRY_POINT: EntryPointRequest = EntryPointRequest::new().with_entry_point(limine_entry);

#[used]
#[link_section = ".limine_reqs"]
static MEMORY_MAP: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[link_section = ".limine_reqs"]
static KERNEL_ELF: KernelFileRequest = KernelFileRequest::new();

#[used]
#[link_section = ".limine_reqs"]
static USER_MODULES: ModuleRequest = ModuleRequest::new();

#[used]
#[link_section = ".limine_reqs"]
static DTB_REQ: DeviceTreeBlobRequest = DeviceTreeBlobRequest::new();

#[used]
#[link_section = ".limine_reqs"]
static HHDM_REQ: HhdmRequest = HhdmRequest::new();

// the kernel's entry point function from the limine bootloader
// limine ensures we are in el1 (kernel mode)
extern "C" fn limine_entry() -> ! {
    // let's see what's in the memory map from limine
    let mmap = MEMORY_MAP
        .get_response()
        .expect("no memory map specified for kernel")
        .entries();

    // emerglogln!("[kernel] printing out memory map");

    // for region in mmap {
    //     emerglogln!("\tfound: {:#018x} - {:#018x} ({} KB) {:?}",
    //         region.base,
    //         region.base + region.len,
    //         region.len >> 10,
    //         region.typ)
    // }

    // TODO: it should be ok if it is empty when -k is passed on the command line
    let modules = USER_MODULES
        .get_response()
        .expect("no modules specified for kernel -- no way to start init")
        .modules();

    let kernel_elf = KERNEL_ELF
        .get_response()
        .expect("no kernel info specified for kernel")
        .file();

    // Set the identity map offset used for fast physical to virtual translations.
    // The offset is only initialized once at startup so it is safe to write directly.
    let hhdm_info = HHDM_REQ
        .get_response()
        .expect("failed to get higher half direct ");
    unsafe {
        super::memory::PHYS_MEM_OFFSET = hhdm_info.offset();
    }
    // Some versions of the limine bootloader place the identity map at the
    // bottom of the higher half range of addresses covered by TTBR1_EL1.
    // This must be taken into account by the MMIO address allocator which
    // starts allocating addresses from the lowest part of the kernel address range.
    #[cfg(not(machine = "bhyve"))]
    if hhdm_info.offset() == *VirtAddr::TTBR1_EL1.start() {
        // the identity map covers the first 4 GB of memory
        const IDENTITY_MAP_SIZE: u64 = 0x1_0000_0000;
        unsafe {
            use super::address::MMIO_RANGE;
            MMIO_RANGE = RangeInclusive::new(
                *MMIO_RANGE.start() + IDENTITY_MAP_SIZE,
                *MMIO_RANGE.end() + IDENTITY_MAP_SIZE,
            );
        }
    }

    // generate generic boot info
    let mut boot_info = Armv8BootInfo {
        memory: alloc::vec![],
        kernel: kernel_elf,
        modules: alloc::vec![],
    };

    // convert memory map from bootloader to memory regions
    let reserved = crate::machine::memory::reserved_regions();
    for mem in mmap.iter() {
        #[allow(unused_assignments)]
        let mut split_range = (None, None);
        let mut skip_region = false;
        // a reserved region of memory may be present in the memory map
        // and Limine may not mark it as so, so we have to modify
        // the memory mapping so that the kernel ignores that region
        for res in reserved {
            if mem.base == res.start.raw() {
                // for now we assume that only one reserved region exists within a single range
                split_range = split(mem, &res);
                if let Some(region) = split_range.0 {
                    boot_info.memory.push(region);
                }
                if let Some(region) = split_range.1 {
                    boot_info.memory.push(region);
                }
                skip_region = true;
                break;
            }
        }
        if !skip_region {
            boot_info.memory.push(MemoryRegion {
                kind: mem.entry_type.into(),
                start: PhysAddr::new(mem.base).unwrap(),
                length: mem.length as usize,
            });
        }
    }

    // function splits a memory region in half based on a reserved region
    fn split(
        memmap: &limine::memory_map::Entry,
        reserved: &MemoryRegion,
    ) -> (Option<MemoryRegion>, Option<MemoryRegion>) {
        let lhs = memmap.base;
        let rhs = memmap.base + memmap.length;

        // case 1: take lhs range
        if reserved.start.raw() == lhs {
            (
                None,
                Some(MemoryRegion {
                    kind: memmap.entry_type.into(),
                    start: PhysAddr::new(memmap.base + reserved.length as u64).unwrap(),
                    length: memmap.length as usize - reserved.length,
                }),
            )
        }
        // case 2: take rhs range
        else if reserved.start.raw() + reserved.length as u64 == rhs {
            (
                Some(MemoryRegion {
                    kind: memmap.entry_type.into(),
                    start: PhysAddr::new(memmap.base).unwrap(),
                    length: memmap.length as usize - reserved.length,
                }),
                None,
            )
        }
        // case 3: split in the middle
        else {
            (
                Some(MemoryRegion {
                    kind: memmap.entry_type.into(),
                    start: PhysAddr::new(memmap.base).unwrap(),
                    length: (reserved.start.raw() - memmap.base) as usize,
                }),
                Some(MemoryRegion {
                    kind: memmap.entry_type.into(),
                    start: PhysAddr::new(reserved.start.raw() + reserved.length as u64).unwrap(),
                    length: (memmap.length
                        - reserved.length as u64
                        - (reserved.start.raw() - memmap.base))
                        as usize,
                }),
            )
        }
    }

    // convert module representation from bootloader to boot module
    boot_info.modules = modules
        .iter()
        .map(|m| BootModule {
            start: VirtAddr::from_ptr(m.addr()),
            length: m.size() as usize,
        })
        .collect();

    crate::kernel_main(&mut boot_info)
}
