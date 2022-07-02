use std::sync::{Arc, RwLock};

use twizzler_abi::device::BusType;
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    request::{RequestDriver, Requester, ResponseInfo, SubmitRequest},
};

struct NvmeController {
    requester: RwLock<Vec<Requester<NvmeQueue>>>,
}

struct NvmeQueue {
    idx: usize,
    ctrl: Arc<NvmeController>,
}

#[derive(Clone, Copy, Debug)]
struct NvmeRequest {
    x: i32,
}

#[derive(Clone, Copy, Debug)]
struct NvmeResponse {
    x: i32,
}

#[async_trait::async_trait]
impl RequestDriver for NvmeQueue {
    type Request = NvmeRequest;
    type Response = NvmeResponse;
    type SubmitError = ();

    async fn submit(
        &self,
        reqs: &[twizzler_driver::request::SubmitRequest<Self::Request>],
    ) -> Result<(), Self::SubmitError> {
        println!("submit called with {:?}", reqs);
        let mut resps = Vec::new();
        for r in reqs {
            let err = if r.id() == 3 { false } else { false };
            resps.push(ResponseInfo::new(
                NvmeResponse { x: r.data().x },
                r.id(),
                err,
            ));
        }
        self.ctrl.requester.read().unwrap()[self.idx].finish(&resps);
        Ok(())
    }

    fn flush(&self) {
        println!("flush called!");
    }

    const NUM_IDS: usize = 8;
}

async fn test<'a>(mut ctrl: Arc<NvmeController>) {
    println!("starting req test");
    let nq = NvmeQueue {
        idx: 0,
        ctrl: ctrl.clone(),
    };
    ctrl.requester.write().unwrap().push(Requester::new(nq));

    let mut reqs = Vec::new();
    for i in 0..10 {
        reqs.push(SubmitRequest::new(NvmeRequest { x: i }));
    }
    let req = ctrl.requester.read().unwrap();
    {
        let inflight = req[0].submit_for_response(&mut reqs).await.unwrap();

        let res = inflight.await;
        println!("got summ {:?}", res);
    }
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

                    let ctrl = Arc::new(NvmeController {
                        requester: RwLock::new(Vec::new()),
                    });
                    twizzler_async::run(test(ctrl));
                }
            }
        }
    }
}
