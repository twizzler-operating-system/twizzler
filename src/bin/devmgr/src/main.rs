use pci_ids::FromId;
use twizzler_abi::kso::{KactionCmd, KactionFlags};
use twizzler_driver::{
    bus::pcie::{PcieFunctionHeader, PcieKactionSpecific},
    device::{BusType, Device},
};

fn get_pcie_offset(bus: u8, device: u8, function: u8) -> usize {
    ((bus as usize * 256) + (device as usize * 8) + function as usize) * 4096
}

fn print_info(bus: u8, slot: u8, function: u8, cfg: &PcieFunctionHeader) -> Option<()> {
    if false {
        println!(
            "{} {} {}:: {:x} {:x} :: {:x} {:x} {:x}",
            bus,
            slot,
            function,
            cfg.vendor_id.get(),
            cfg.device_id.get(),
            cfg.class.get(),
            cfg.subclass.get(),
            cfg.progif.get(),
        );
    }
    let device = pci_ids::Device::from_vid_pid(cfg.vendor_id.get(), cfg.device_id.get())?;
    let vendor = device.vendor();
    let class = pci_ids::Class::from_id(cfg.class.get())?;
    //let subclass = pci_ids::Class::from_id(cfg.subclass.get())?;
    println!(
        "[devmgr] {:02x}:{:02x}.{:02x} {}: {} {}",
        bus,
        slot,
        function,
        class.name(),
        vendor.name(),
        device.name()
    );

    None
}

fn start_pcie_device(seg: &Device, bus: u8, device: u8, function: u8) {
    let _ = seg.kaction(
        KactionCmd::Specific(PcieKactionSpecific::RegisterDevice.into()),
        ((bus as u64) << 16) | ((device as u64) << 8) | (function as u64),
        KactionFlags::empty(),
    );
}

fn start_pcie(seg: Device) {
    println!("[devmgr] scanning PCIe bus");
    //let info = unsafe { bus.get_info::<PcieInfo>(0).unwrap() };
    let mmio = seg.get_mmio(0).unwrap();

    for bus in 0..=255 {
        for device in 0..32 {
            let off = get_pcie_offset(bus, device, 0);
            let cfg = unsafe { mmio.get_mmio_offset::<PcieFunctionHeader>(off) };
            if cfg.vendor_id.get() != 0xffff
                && cfg.device_id.get() != 0xffff
                && cfg.vendor_id.get() != 0
            {
                let mf = if cfg.header_type.get() & 0x80 != 0 {
                    7
                } else {
                    0
                };
                for function in 0..=mf {
                    let off = get_pcie_offset(bus, device, function);
                    let cfg = unsafe { mmio.get_mmio_offset::<PcieFunctionHeader>(off) };
                    if cfg.vendor_id.get() != 0xffff {
                        print_info(bus, device, function, cfg);
                        start_pcie_device(&seg, bus, device, function)
                    }
                }
            }
        }
    }
}

fn main() {
    println!("[devmgr] starting device manager");
    let device_root = twizzler_driver::device::get_bustree_root();
    for device in device_root.children() {
        if device.is_bus() && device.bus_type() == BusType::Pcie {
            start_pcie(device);
        }
    }
}
