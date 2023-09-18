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
    kernel: &'static File,

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

impl From<MemoryMapEntryType> for MemoryRegionKind {
    fn from(st: MemoryMapEntryType) -> Self {
        match st {
            MemoryMapEntryType::Usable => MemoryRegionKind::UsableRam,
            MemoryMapEntryType::KernelAndModules => MemoryRegionKind::BootloaderReserved,
            _ => MemoryRegionKind::Reserved,
        }
    }
}

#[used]
static ENTRY_POINT: EntryPointRequest = EntryPointRequest::new(0)
    .entry(Ptr::new(limine_entry));

#[used]
static MEMORY_MAP: MemmapRequest = MemmapRequest::new(0);

#[used]
static KERNEL_ELF: KernelFileRequest = KernelFileRequest::new(0);

#[used]
static USER_MODULES: ModuleRequest = ModuleRequest::new(0);


#[link_section = ".limine_reqs"]
#[used]
static LR1: &'static EntryPointRequest = &ENTRY_POINT;

#[link_section = ".limine_reqs"]
#[used]
static LR2: &'static MemmapRequest = &MEMORY_MAP;

#[link_section = ".limine_reqs"]
#[used]
static LR3: &'static KernelFileRequest = &KERNEL_ELF;

#[link_section = ".limine_reqs"]
#[used]
static LR4: &'static ModuleRequest = &USER_MODULES;

// the kernel's entry point function from the limine bootloader
// limine ensures we are in el1 (kernel mode)
fn limine_entry() -> ! {
    // let's see what's in the memory map from limine
    let mmap = MEMORY_MAP
        .get_response() // Ptr<MemmapResponse>
        .get() // Option<'static T>
        .expect("no memory map specified for kernel") // MemmapResponse
        .memmap(); // &[NonNullPtr<MemmapEntry>]

    // emerglogln!("[kernel] printing out memory map");

    // for region in mmap {
    //     emerglogln!("\tfound: {:#018x} - {:#018x} ({} KB) {:?}",
    //         region.base,
    //         region.base + region.len,
    //         region.len >> 10,
    //         region.typ)
    // }

    // TODO: it should be ok if it is empty when -k is passed on the command line
    let modules =  USER_MODULES
        .get_response()
        .get()
        .expect("no modules specified for kernel -- no way to start init")
        .modules();

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
        modules: alloc::vec![],
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

    // convert module representation from bootloader to boot module
    boot_info.modules = modules
    .iter()
    .map(|m| BootModule {
        start: VirtAddr::new(m.base.as_ptr().unwrap() as u64).unwrap(),
        length: m.length as usize,
    })
    .collect();

    crate::kernel_main(&mut boot_info)
}
