use alloc::format;
use twizzler_abi::device::CacheType;
use twizzler_abi::device::bus::pcie::{PcieInfo, PcieKactionSpecific};
use twizzler_abi::{
    device::BusType,
    kso::{KactionError, KactionValue},
};
use x86_64::PhysAddr;

use crate::{arch, device::DeviceRef};

fn register_device(seg: u16, bus: u8, device: u8, function: u8) -> Option<DeviceRef> {
    let acpi = arch::acpi::get_acpi_root();
    let cfg = acpi::mcfg::PciConfigRegions::new(acpi).ok()?;
    let _addr = cfg.physical_address(seg, bus, device, function)?;

    todo!()
}

fn kaction(_device: DeviceRef, cmd: u32, _arg: u64) -> Result<KactionValue, KactionError> {
    let cmd: PcieKactionSpecific = cmd.try_into()?;
    match cmd {
        PcieKactionSpecific::RegisterDevice => todo!(),
        PcieKactionSpecific::AllocateInterrupt => todo!(),
    }
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
    dev.add_mmio(addr, end_addr, CacheType::Uncachable);
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
        }
    }
}
