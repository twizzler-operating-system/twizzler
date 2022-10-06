use core::arch::asm;

use crate::{
    initrd::BootModule,
    memory::{MemoryRegion, VirtAddr},
    BootInfo,
};

pub enum BootInfoSystemTable {
    Unknown
}

struct Armv8BootInfo;

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


#[no_mangle]
pub extern "C" fn _start() -> ! {
    // let's set the stack
    unsafe { asm!(
        "ldr x30, =__stack_top",
        "mov sp, x30"
    );}

    crate::arch::kernel_main();
    // crate::kernel_main(&mut Armv8BootInfo {})
}
