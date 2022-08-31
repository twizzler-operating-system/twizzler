use std::sync::{Arc, RwLock};

use twizzler_abi::device::{BusType, MailboxPriority};
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    request::{RequestDriver, Requester, ResponseInfo, SubmitRequest},
    DeviceController,
};

struct NvmeController {
    requester: RwLock<Vec<Requester<NvmeQueue>>>,
    device_ctrl: DeviceController,
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
    #[allow(dead_code)]
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

async fn test3<'a>(ctrl: Arc<NvmeController>) {
    let int = ctrl.device_ctrl.events().allocate_interrupt().unwrap();
}

async fn test2<'a>(ctrl: Arc<NvmeController>) {
    loop {
        let (mp, msg) = ctrl
            .device_ctrl
            .events()
            .next_msg(MailboxPriority::Idle)
            .await;
        println!("mailbox message: {:?} {}", mp, msg);
    }
}

async fn test1<'a>(ctrl: Arc<NvmeController>) {
    println!("submitting a mailbox message");
    ctrl.device_ctrl
        .device()
        .repr()
        .submit_mailbox_msg(MailboxPriority::Low, 1234);
    //   println!("starting req test");

    /*
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
    */
}

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

                    let ctrl = Arc::new(NvmeController {
                        requester: RwLock::new(Vec::new()),
                        device_ctrl: DeviceController::new_from_device(child),
                    });
                    let c1 = ctrl.clone();
                    let c2 = ctrl.clone();
                    let c3 = ctrl.clone();
                    std::thread::spawn(|| {
                        twizzler_async::run(test1(c1));
                    });
                    std::thread::spawn(|| {
                        twizzler_async::run(test2(c2));
                    });
                    std::thread::spawn(|| {
                        twizzler_async::run(test3(c3));
                    });
                }
            }
        }
    }
}
