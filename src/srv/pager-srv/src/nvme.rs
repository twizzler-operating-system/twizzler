use core::panic;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_executor::Executor;
use twizzler_abi::device::BusType;
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo, device::Device, dma::DMA_PAGE_SIZE, DeviceController,
};

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

            const NR: usize = 128;
            const END: usize = 1024 * 1024 * 1024 * 100;
            if true {
                let mut last_msg = Instant::now();
                for offset in (0..END).step_by(DMA_PAGE_SIZE * NR) {
                    let page = offset / (DMA_PAGE_SIZE * NR);
                    if page % 100 == 0 {
                        let now = Instant::now();
                        if now.duration_since(last_msg) > Duration::from_secs(1) {
                            last_msg = now;
                            println!(
                                "written {} KB / {} KB ({}%)",
                                offset / 1024,
                                END / 1024,
                                (offset * 100) / END
                            );
                        }
                    }
                    let mut buf = [(page % 97) as u8; DMA_PAGE_SIZE * NR];
                    let lbas_per_page = DMA_PAGE_SIZE / ctrl.blocking_get_lba_size();
                    let lba = page * lbas_per_page * NR;
                    ctrl.blocking_write_pages::<NR>(lba as u64, &mut buf)
                        .unwrap();
                }
            }

            println!("reading back...");
            let mut last_msg = Instant::now();
            for offset in (0..END).step_by(DMA_PAGE_SIZE * NR) {
                let page = offset / (DMA_PAGE_SIZE * NR);
                if page % 100 == 0 {
                    let now = Instant::now();
                    if now.duration_since(last_msg) > Duration::from_secs(1) {
                        last_msg = now;
                        println!(
                            "written {} KB / {} KB ({}%)",
                            offset / 1024,
                            END / 1024,
                            (offset * 100) / END
                        );
                    }
                }
                let should_be_buf = [((offset / (DMA_PAGE_SIZE * NR)) % 97) as u8; 0x1000];
                let mut buf = [0; DMA_PAGE_SIZE];
                let lbas_per_page = DMA_PAGE_SIZE * NR / ctrl.blocking_get_lba_size();
                let lba = page * lbas_per_page;
                ctrl.blocking_read_page(lba as u64, &mut buf, 0).unwrap();
                assert_eq!(should_be_buf, buf);
            }
            loop {}
            return Some(ctrl);
        }
    }

    None
}
