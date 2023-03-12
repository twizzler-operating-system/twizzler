use core::panic;
use std::sync::{Arc, Mutex, RwLock};

use twizzler_abi::device::BusType;
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    dma::{DmaOptions, DmaPool},
    DeviceController,
};

mod controller;
mod dma;
mod queue;
mod requester;

pub use controller::NvmeController;

pub fn init_nvme() -> Arc<NvmeController> {
    let device_root = twizzler_driver::get_bustree_root();
    for device in device_root.children() {
        if device.is_bus() && device.bus_type() == BusType::Pcie {
            for child in device.children() {
                let info = unsafe { child.get_info::<PcieDeviceInfo>(0).unwrap() };
                if info.get_data().class == 1
                    && info.get_data().subclass == 8
                    && info.get_data().progif == 2
                {
                    println!(
                        "found nvme controller {:x}.{:x}.{:x}",
                        info.get_data().bus_nr,
                        info.get_data().dev_nr,
                        info.get_data().func_nr
                    );

                    let mut ctrl = Arc::new(NvmeController::new(
                        DeviceController::new_from_device(child),
                    ));
                    controller::init_controller(&mut ctrl);
                    return ctrl;
                }
            }
        }
    }
    panic!("no nvme controller found");
}
