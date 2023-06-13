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

    /// A reference to the kernel's ELF file in memory.
    /// 
    /// This contains other useful information such as the kernel's
    /// command line parameters.
    kernel: &'static LimineFile,
}

impl BootInfo for Armv8BootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion] {
        unsafe { core::intrinsics::transmute(&self.memory[..]) }
    }

    fn get_modules(&self) -> &'static [BootModule] {
        todo!("get modules")
    }

    fn kernel_image_info(&self) -> (VirtAddr, usize) {
        (
            VirtAddr::from_ptr(self.kernel.base.as_ptr().unwrap()),
            self.kernel.length as usize,
        )
    }

    fn get_system_table(&self, _table: BootInfoSystemTable) -> VirtAddr {
        todo!("get system table")
    }

    fn get_cmd_line(&self) -> &'static str {
        if let Some(cmd) = self.kernel.cmdline.as_ptr() {
            let ptr = cmd as *const u8;
            const MAX_CMD_LINE_LEN: usize = 0x1000;
            let slice = unsafe { 
                core::slice::from_raw_parts(ptr, MAX_CMD_LINE_LEN) 
            };
            let slice = &slice[
                0..slice
                    .iter()
                    .position(|r| *r == 0)
                    .unwrap_or(0)
            ];
            core::str::from_utf8(slice).unwrap()
        } else {
            ""
        }
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

#[used]
static KERNEL_ELF: LimineKernelFileRequest = LimineKernelFileRequest::new(0);

#[link_section = ".limine_reqs"]
#[used]
static LR1: &'static LimineEntryPointRequest = &ENTRY_POINT;

#[link_section = ".limine_reqs"]
#[used]
static LR2: &'static LimineMmapRequest = &MEMORY_MAP;

#[link_section = ".limine_reqs"]
#[used]
static LR3: &'static LimineKernelFileRequest = &KERNEL_ELF;

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

    let kernel_elf = unsafe {
        KERNEL_ELF
            .get_response()
            .get()
            .expect("no kernel info specified for kernel")
            .kernel_file
            .as_ptr()
            .unwrap()
            .as_ref()
            .unwrap()
    };

    // generate generic boot info
    let mut boot_info = Armv8BootInfo {
        memory: alloc::vec![],
        kernel: kernel_elf,
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
