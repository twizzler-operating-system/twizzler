use std::sync::{Arc, OnceLock};

use twizzler_driver::{bus::pcie::PcieDeviceInfo, device::Device};

pub(crate) mod controller;
mod dma;
mod requester;

pub use controller::{current_queue_sleep, reap_current_queue, NvmeController};
use twizzler_rt_abi::error::{NamingError, TwzError};

/// Set once at probe so the watchdog, which owns no pager state, can reach the controller.
static CTRL: OnceLock<Arc<NvmeController>> = OnceLock::new();

/// Dump controller state. No-op before the controller exists.
pub fn dump_stall() {
    if let Some(ctrl) = CTRL.get() {
        ctrl.dump_stall();
    }
}

/// Heartbeat form of the per-queue interrupt state. See `NvmeController::queue_diag`.
pub fn queue_diag() {
    if let Some(ctrl) = CTRL.get() {
        ctrl.queue_diag();
    }
}

pub fn init_nvme() -> Result<Arc<NvmeController>, TwzError> {
    let devices = devmgr::enumerate_devices(devmgr::DriverSpec {
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

            let ctrl = Arc::new(NvmeController::new(device)?);
            let _ = CTRL.set(ctrl.clone());
            return Ok(ctrl);
        }
    }
    Err(NamingError::NotFound.into())
}
