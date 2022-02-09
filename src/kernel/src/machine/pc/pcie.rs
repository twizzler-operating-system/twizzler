use acpi::{sdt::Signature, PhysicalMapping};
use alloc::format;
use twizzler_abi::device::bus::pcie::PcieInfo;
use twizzler_abi::{
    device::{BusType, DeviceType},
    kso::{KactionError, KactionValue},
};
use x86_64::{PhysAddr, VirtAddr};

use crate::{
    arch,
    device::{Device, DeviceRef},
};

fn kaction(device: DeviceRef) -> Result<KactionValue, KactionError> {
    todo!()
}

// TODO: we can't just assume every segment has bus 0..255.
fn init_segment(seg: u16, addr: PhysAddr) {
    let dev = crate::device::create_busroot(&format!("pcie_root({})", seg), BusType::Pcie, kaction);
    let end_addr = addr + (255u64 << 20 | 32 << 15 | 8 << 12);
    let info = PcieInfo {
        bus_start: 0,
        bus_end: 0xff,
        seg_nr: seg,
    };
    dev.add_info(&info);
    dev.add_mmio(addr, end_addr);
}

pub(super) fn init() {
    logln!("[kernel::machine::pcie] init");

    let acpi = arch::acpi::get_acpi_root();

    let cfg =
        acpi::mcfg::PciConfigRegions::new(acpi).expect("failed to get PCIe configuration regions");
    for seg in 0..0xffff {
        let addr = cfg.physical_address(seg, 0, 0, 0);
        if let Some(addr) = addr {
            init_segment(seg, PhysAddr::new(addr));
            logln!("found pcie {:x}", addr);
        }
    }

    logln!("done");
    loop {}
}
