use super::super::common::mmio::map_device_region;

pub(super) fn init() {
    let Some((ecam, len)) = super::info::get_pcie_ecam() else {
        log::warn!("no pci-host-ecam-generic node in the device tree; no PCIe");
        return;
    };
    let ecam_va = map_device_region(ecam, len);
    let msi_addr = super::interrupt::msi_frame().map_or(0, |(doorbell, _, _)| doorbell);
    crate::machine::pcie::init_segment(0, ecam, ecam_va, msi_addr);
}
