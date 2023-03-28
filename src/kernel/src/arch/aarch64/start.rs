use limine::*;

use crate::{
    initrd::BootModule,
    memory::{MemoryRegion, VirtAddr},
    BootInfo,
};

pub enum BootInfoSystemTable {
    Unknown
}

pub struct Armv8BootInfo;

impl BootInfo for Armv8BootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion] {
        todo!()
    }

    fn get_modules(&self) -> &'static [BootModule] {
        todo!()
    }

    fn kernel_image_info(&self) -> (VirtAddr, usize) {
        todo!()
    }

    fn get_system_table(&self, _table: BootInfoSystemTable) -> VirtAddr {
        todo!()
    }

    fn get_cmd_line(&self) -> &'static str {
        todo!()
    }
}

#[used]
static ENTRY_POINT: LimineEntryPointRequest = LimineEntryPointRequest::new(0)
    .entry(LiminePtr::new(limine_entry));

#[link_section = ".limine_reqs"]
#[used]
static LR1: &'static LimineEntryPointRequest = &ENTRY_POINT;

// the kernel's entry point function from the limine bootloader
// limine ensures we are in el1 (kernel mode)
fn limine_entry() -> ! {
    // let's do something more interesting
    crate::arch::kernel_main()
}
