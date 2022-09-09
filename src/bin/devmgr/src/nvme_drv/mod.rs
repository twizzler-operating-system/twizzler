use std::{future, time::Duration};

use twizzler_abi::device::BusType;

use twizzler_async::Task;
use twizzler_driver::bus::pcie::PcieDeviceInfo;

use self::controller::NvmeController;

mod controller;
mod memory;
mod queue;

#[allow(dead_code)]
pub fn start() {
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

                    let ctrl = NvmeController::new(child);

                    let _task = Task::spawn(async move {
                        let res = twizzler_async::timeout_after(
                            ctrl.init_controller(),
                            Duration::from_millis(1000),
                        )
                        .await;

                        if res.is_some() {
                            let res = twizzler_async::timeout_after(
                                ctrl.identify_controller(),
                                Duration::from_millis(200),
                            )
                            .await;
                            println!("{:?}", res);
                        }
                    });

                    twizzler_async::run(future::pending::<()>());
                }
            }
        }
    }
}
