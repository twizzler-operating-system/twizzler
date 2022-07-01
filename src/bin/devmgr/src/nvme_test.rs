use twizzler_abi::device::BusType;
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    request::{RequestDriver, Requester},
};

struct NvmeController {}

struct NvmeQueue {}

#[derive(Clone, Copy, Debug)]
struct NvmeRequest {}

impl RequestDriver for NvmeQueue {
    type Request = NvmeRequest;
    type Response = ();

    type SubmitError = ();

    fn allocate(
        &self,
        len: usize,
    ) -> Result<twizzler_driver::request::CircularRange, Self::SubmitError> {
        todo!()
    }

    fn submit(
        &self,
        reqs: &[Self::Request],
        ids: twizzler_driver::request::CircularRange,
    ) -> Result<twizzler_driver::request::CircularRange, Self::SubmitError> {
        todo!()
    }

    fn flush(&self) {
        todo!()
    }
}

async fn test() {
    let nq = NvmeQueue {};
    let req = NvmeRequest {};
    let eng = Requester::new(nq);

    let inflight = eng.submit(&[req]).await.unwrap();

    let res = inflight.await;
}

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
                }
            }
        }
    }
}
