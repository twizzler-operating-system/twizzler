use std::env::args;

use pci_ids::FromId;
use twizzler_abi::{
    kso::{KactionCmd, KactionFlags},
    syscall::{ThreadSync, ThreadSyncReference, ThreadSyncWake},
};
use twizzler_driver::{
    bus::pcie::{PcieFunctionHeader, PcieKactionSpecific},
    device::{BusType, Device},
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use volatile::{access::ReadOnly, map_field, VolatilePtr};

//mod nvme_test;

fn get_pcie_offset(bus: u8, device: u8, function: u8) -> usize {
    ((bus as usize * 256) + (device as usize * 8) + function as usize) * 4096
}

fn print_info<'a>(
    bus: u8,
    slot: u8,
    function: u8,
    cfg: VolatilePtr<'a, PcieFunctionHeader, ReadOnly>,
) -> Option<()> {
    if false {
        println!(
            "{} {} {}:: {:x} {:x} :: {:x} {:x} {:x}",
            bus,
            slot,
            function,
            map_field!(cfg.vendor_id).read(),
            map_field!(cfg.device_id).read(),
            map_field!(cfg.class).read(),
            map_field!(cfg.subclass).read(),
            map_field!(cfg.progif).read(),
        );
    }
    let device = pci_ids::Device::from_vid_pid(
        map_field!(cfg.vendor_id).read(),
        map_field!(cfg.device_id).read(),
    )?;
    let vendor = device.vendor();
    let class = pci_ids::Class::from_id(map_field!(cfg.class).read())?;
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
    let kr = seg.kaction(
        KactionCmd::Specific(PcieKactionSpecific::RegisterDevice.into()),
        ((bus as u64) << 16) | ((device as u64) << 8) | (function as u64),
        KactionFlags::empty(),
        0,
    );
    match kr {
        Ok(_) => {}
        Err(_) => eprintln!(
            "failed to register pcie device {:x}.{:x}.{:x}",
            bus, device, function
        ),
    }
}

fn start_pcie(seg: Device) {
    println!("[devmgr] scanning PCIe bus");
    //let info = unsafe { bus.get_info::<PcieInfo>(0).unwrap() };
    let mmio = seg.get_mmio(0).unwrap();

    for bus in 0..=255 {
        for device in 0..32 {
            let off = get_pcie_offset(bus, device, 0);
            let cfg = unsafe { mmio.get_mmio_offset::<PcieFunctionHeader>(off) };
            let cfg = cfg.as_ptr();
            if map_field!(cfg.vendor_id).read() != 0xffff
                && map_field!(cfg.device_id).read() != 0xffff
                && map_field!(cfg.vendor_id).read() != 0
            {
                let mf = if map_field!(cfg.header_type).read() & 0x80 != 0 {
                    7
                } else {
                    0
                };
                for function in 0..=mf {
                    let off = get_pcie_offset(bus, device, function);
                    let cfg = unsafe { mmio.get_mmio_offset::<PcieFunctionHeader>(off) };
                    let cfg = cfg.as_ptr();
                    if map_field!(cfg.vendor_id).read() != 0xffff {
                        print_info(bus, device, function, cfg);
                        start_pcie_device(&seg, bus, device, function)
                    }
                }
            }
        }
    }
}

extern crate twizzler_runtime;
fn main() {
    println!("[devmgr] starting device manager {:?}", args());
    logboi::LogHandle::new()
        .unwrap()
        .log("Hello Logboi, from devmgr!".as_bytes());
    let id = args().into_iter().nth(1).unwrap().parse::<u128>().unwrap();
    let obj = Object::<std::sync::atomic::AtomicU64>::init_id(
        ObjID::new(id),
        Protections::WRITE | Protections::READ,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let device_root = twizzler_driver::get_bustree_root();
    for device in device_root.children() {
        if device.is_bus() && device.bus_type() == BusType::Pcie {
            start_pcie(device);
        }
    }

    //nvme_test::start();

    let base = unsafe { obj.base_unchecked() };
    base.store(1, std::sync::atomic::Ordering::SeqCst);
    twizzler_abi::syscall::sys_thread_sync(
        &mut [ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(base),
            usize::MAX,
        ))],
        None,
    )
    .unwrap();
}
