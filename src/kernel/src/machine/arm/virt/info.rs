use fdt::Fdt;

use crate::once::Once;
use crate::BootInfo;
use crate::arch::BootInfoSystemTable;

// We use device tree to describe the hardware on this machine
static FDT: Once<Fdt<'static>> = Once::new();

pub fn init<B: BootInfo>(boot_info: &B) {
    FDT.call_once(|| {
        let dtb = {
            // try to find device tree location using the bootloader
            let bootloader_dtb_addr = boot_info.get_system_table(BootInfoSystemTable::Dtb);
            // otherwise use a static address
            if bootloader_dtb_addr.raw() == 0 {
                // in the case of QEMU's virt platform, we can use 0x4000_0000
                super::memory::DTB_ADDR.kernel_vaddr()
            } else {
                bootloader_dtb_addr
            }    
        };
        // should not fail, but it might ...
        unsafe {
            Fdt::from_ptr(dtb.as_ptr())
                .expect("invalid DTB file, cannot boot")
        }
    });
}

pub fn devicetree() -> &'static Fdt<'static> {
    FDT.poll()
        .expect("device tree initialization has not been called!!")
}
