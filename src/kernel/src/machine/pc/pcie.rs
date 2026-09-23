use crate::{arch, arch::memory::phys_to_virt, memory::PhysAddr};

pub(super) fn init() {
    log::debug!("pcie init");

    let acpi = arch::acpi::get_acpi_root();

    let cfg =
        acpi::mcfg::PciConfigRegions::new(acpi).expect("failed to get PCIe configuration regions");
    for seg in 0..0xffff {
        let addr = cfg.physical_address(seg, 0, 0, 0);
        if let Some(addr) = addr {
            let addr = PhysAddr::new(addr).unwrap();
            crate::machine::pcie::init_segment(seg, addr, phys_to_virt(addr), 0);
        }
    }
}
