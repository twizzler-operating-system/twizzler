use fdt::{node::FdtNode, Fdt};
use twizzler_abi::device::{CacheType, MmioInfo};

use crate::{arch::BootInfoSystemTable, once::Once, BootInfo};

// We use device tree to describe the hardware on this machine
static FDT: Once<Fdt<'static>> = Once::new();

pub fn init<B: BootInfo>(boot_info: &B) {
    // TODO: fix device tree parsing. All nodes are not shown.
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
        // this should not fail, but it might due a bad magic value
        // in the FDT header or the fact that a NULL pointer is passed in.
        unsafe { Fdt::from_ptr(dtb.as_ptr()).expect("invalid DTB file, cannot boot") }
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
    // TODO: get this information from the device tree.
    use super::memory::BHYVE_UART;
    mmio.length = BHYVE_UART.length as u64;

    // set things according to bhyve
    clock_freq = 0x16e3600;
    mmio.info = BHYVE_UART.start.raw();
    (clock_freq, mmio)
}

// Retrieve the interrupt number for the UART device
pub fn get_uart_interrupt_num() -> Option<u32> {
    // NOTE: this encoding is specific to GIC interrupt controllers
    // TODO: use device tree or hard code this.
    None
}

/// Return the MMIO info for the GICv3 registers
pub fn get_gicv3_info() -> (MmioInfo, MmioInfo) {
    use super::memory::{BHYVE_GICD, BHYVE_GICR};
    let gicd_mmio = MmioInfo {
        length: BHYVE_GICD.length as u64,
        cache_type: CacheType::MemoryMappedIO,
        info: BHYVE_GICD.start.raw(),
    };
    let gicr_mmio = MmioInfo {
        length: BHYVE_GICR.length as u64,
        cache_type: CacheType::MemoryMappedIO,
        info: BHYVE_GICR.start.raw(),
    };
    (gicd_mmio, gicr_mmio)
}

fn recursive_compat_finder<'a, 'b>(
    parent: &FdtNode<'b, 'a>,
    compat: &str,
) -> Option<FdtNode<'b, 'a>> {
    // first check every child this node has
    for c in parent.children() {
        // get compatability property string
        if let Some(compatability) = c.compatible() {
            emerglogln!("checking: {}", c.name);
            // check if any match our desired compatability string
            let is_a_match = compatability.all().any(|cp| cp == compat);
            if is_a_match {
                return Some(c);
            }
        } else {
            emerglogln!("passsed: {}", c.name);
        }
        // check the children of every child
        if let Some(child_match) = recursive_compat_finder(&c.clone(), compat) {
            return Some(child_match.clone());
        }
    }
    None
}

// library has something like this ...
pub fn find_compatable<'a, 'b>(compat: &str) -> Option<FdtNode<'b, 'a>> {
    let fdt = devicetree();
    // iterate over every node in the device tree
    let root = fdt.find_node("/").expect("could not get root node");
    recursive_compat_finder(&root, compat)
}

fn _print_devtree(node: &FdtNode<'_, '_>, n_spaces: usize) {
    for _ in 0..n_spaces {
        emerglog!("  ");
    }
    emerglogln!("{}", node.name);
    // print all children
    for c in node.children() {
        _print_devtree(&c, n_spaces + 1);
    }
}

pub fn print_device_tree() {
    let fdt = devicetree();
    let mut root = fdt.find_node("/").unwrap();
    _print_devtree(&root, 0);
}
