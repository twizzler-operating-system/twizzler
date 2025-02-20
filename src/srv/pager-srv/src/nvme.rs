use core::panic;
use std::sync::Arc;

use async_executor::Executor;
use twizzler_abi::device::BusType;
use twizzler_driver::{bus::pcie::PcieDeviceInfo, device::Device, DeviceController};

mod controller;
mod dma;
mod requester;

pub use controller::NvmeController;

pub async fn init_nvme(ex: &'static Executor<'static>) -> Option<Arc<NvmeController>> {
    let devices = devmgr::get_devices(devmgr::DriverSpec {
        supported: devmgr::Supported::PcieClass(1, 8, 2),
    })?;

    for device in &devices {
        let device = Device::new(device.id).ok();
        if let Some(device) = device {
            let info = unsafe { device.get_info::<PcieDeviceInfo>(0).unwrap() };
            tracing::info!(
                "found nvme controller at {:02x}:{:02x}.{:02x}",
                info.get_data().bus_nr,
                info.get_data().dev_nr,
                info.get_data().func_nr
            );

            let ctrl = Arc::new(NvmeController::new(device).ok()?);
            //let ident = ctrl.identify_controller().await.unwrap();
            //tracing::info!("ident: {:?}", ident);

            let mut buf = [0; 0x1000];
            ctrl.blocking_read_page(0, &mut buf, 0).unwrap();

            return Some(ctrl);
        }
    }

    None
}
