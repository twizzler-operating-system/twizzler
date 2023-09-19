use fdt::Fdt;

use twizzler_abi::device::{MmioInfo, CacheType};

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

// return the clock frequency and the mmio register info
pub fn get_uart_info() -> (usize, MmioInfo) {
    let mut mmio = MmioInfo {
        length: 0,
        cache_type: CacheType::MemoryMappedIO,
        info: 0,
    };
    let mut clock_freq: usize = 0;
    // we use the device tree to retrieve mmio register information
    // and other useful configuration info
    let chosen = devicetree().chosen();
    if let Some(uart) = chosen.stdout() {
        // find the mmio registers
        let regs = uart.reg().unwrap().next().unwrap();
        mmio.info = regs.starting_address as u64;
        mmio.length = regs.size.unwrap() as u64;
        // find the clock information
        if let Some(clock_list) = uart.property("clocks") {
            let phandle: u32 = {
                // TODO: use size cell/address cell info
                let mut converter = [0u8; 4];
                let mut phandle = 0;
                for (i, v) in clock_list.value.iter().enumerate() {
                    converter[i % 4] = *v;
                    if (i + 1) % core::mem::size_of::<u32>() == 0 {
                        // converted value
                        phandle = u32::from_be_bytes(converter);
                        break;
                    }
                }
                phandle
            };
            if let Some(clock) = devicetree().find_phandle(phandle) {
                clock_freq = clock.property("clock-frequency").unwrap().as_usize().unwrap();
            }
        }
    }
    (clock_freq, mmio)
}
