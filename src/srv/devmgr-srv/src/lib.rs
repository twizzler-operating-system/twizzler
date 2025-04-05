#![feature(naked_functions)]

use devmgr::{DriverSpec, OwnedDevice};
use pci_types::device_type::DeviceType;
use twizzler::{
    collections::vec::VecObject,
    object::{ObjID, ObjectBuilder},
};
use twizzler_abi::kso::{KactionCmd, KactionFlags};
use twizzler_driver::{
    bus::pcie::{PcieDeviceInfo, PcieFunctionHeader, PcieKactionSpecific},
    device::{BusType, Device},
};
use twizzler_rt_abi::error::TwzError;
use volatile::map_field;

fn get_pcie_offset(bus: u8, device: u8, function: u8) -> usize {
    ((bus as usize * 256) + (device as usize * 8) + function as usize) * 4096
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
    tracing::info!("[devmgr] scanning PCIe bus");
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
                        let dt = DeviceType::from((
                            map_field!(cfg.class).read(),
                            map_field!(cfg.subclass).read(),
                        ));
                        tracing::info!(
                            "pcie device: {:02x}:{:02x}.{:02x} -- {:?}",
                            bus,
                            device,
                            function,
                            dt
                        );
                        start_pcie_device(&seg, bus, device, function)
                    }
                }
            }
        }
    }
}

#[secgate::secure_gate]
pub fn devmgr_start() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    let device_root = twizzler_driver::get_bustree_root();
    for device in device_root.children() {
        if device.is_bus() && device.bus_type() == BusType::Pcie {
            start_pcie(device);
        }
    }
}

#[secgate::secure_gate]
pub fn get_devices(spec: DriverSpec) -> Result<ObjID, TwzError> {
    match spec.supported {
        devmgr::Supported::PcieClass(class, subclass, progif) => {
            let device_root = twizzler_driver::get_bustree_root();
            let mut ids = Vec::new();
            for device in device_root.children() {
                if device.is_bus() && device.bus_type() == BusType::Pcie {
                    for child in device.children() {
                        let info = unsafe { child.get_info::<PcieDeviceInfo>(0).unwrap() };
                        if info.get_data().class == class
                            && info.get_data().subclass == subclass
                            && info.get_data().progif == progif
                        {
                            ids.push(child.id());
                        }
                    }
                }
            }

            tracing::debug!("found devices {:?} for spec {:?}", ids, spec);
            let mut owned_devices_object = VecObject::new(ObjectBuilder::default())?;
            for id in ids {
                owned_devices_object.push(OwnedDevice { id })?;
            }
            // TODO: on-drop for this object.
            Ok(owned_devices_object.object().id())
        }
    }
}
