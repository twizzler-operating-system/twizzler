use std::{
    mem::size_of,
    sync::{Arc, RwLock},
};

use nvme::ds::{
    controller::properties::config::ControllerConfig,
    queue::{comentry::CommonCompletion, subentry::CommonCommand},
};
use twizzler_abi::device::{BusType, MailboxPriority};
use twizzler_async::block_on;
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    dma::{DeviceSync, DmaOptions, DmaPool},
    request::{RequestDriver, Requester, ResponseInfo, SubmitRequest},
    DeviceController,
};
use volatile_cell::VolatileCell;

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

fn init_controller(ctrl: &mut Arc<NvmeController>) {
    let bar = ctrl.device_ctrl.device().get_mmio(1).unwrap();
    let reg =
        unsafe { bar.get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0) };

    let config = ControllerConfig::new();
    reg.configuration.set(config);

    while reg.status.get().ready() {
        core::hint::spin_loop();
    }
    println!("version: {} {}", reg.version_maj(), reg.version_min());

    let aqa = nvme::ds::controller::properties::aqa::AdminQueueAttributes::new()
        .with_completion_queue_size(32)
        .with_submission_queue_size(32);
    reg.admin_queue_attr.set(aqa);

    let dma = DmaPool::new(
        DmaPool::default_spec(),
        twizzler_driver::dma::Access::BiDirectional,
        DmaOptions::empty(),
    );
    let mut saq = dma
        .allocate_array(32, nvme::ds::queue::subentry::CommonCommand::default())
        .unwrap();
    let mut caq = dma
        .allocate_array(32, nvme::ds::queue::comentry::CommonCompletion::default())
        .unwrap();

    println!("{} {}", saq.num_bytes(), caq.num_bytes());

    let cpin = caq.pin().unwrap();
    let spin = saq.pin().unwrap();

    assert_eq!(cpin.len(), 1);
    assert_eq!(spin.len(), 1);

    let cpin_addr = cpin[0].addr();
    let spin_addr = spin[0].addr();

    reg.admin_comqueue_base_addr.set(cpin_addr.into());
    reg.admin_subqueue_base_addr.set(spin_addr.into());

    //let css_nvm = reg.capabilities.get().supports_nvm_command_set();
    //let css_more = reg.capabilities.get().supports_more_io_command_sets();
    // TODO: check bit 7 of css.

    let config = ControllerConfig::new()
        .with_enable(true)
        .with_io_completion_queue_entry_size(
            size_of::<CommonCompletion>()
                .next_power_of_two()
                .log2()
                .try_into()
                .unwrap(),
        )
        .with_io_submission_queue_entry_size(
            size_of::<CommonCommand>()
                .next_power_of_two()
                .log2()
                .try_into()
                .unwrap(),
        );

    reg.configuration.set(config);
    while !reg.status.get().ready() {
        core::hint::spin_loop();
    }
    println!("controller init");

    let ident = dma
        .allocate(nvme::ds::identify::controller::IdentifyControllerDataStructure::default())
        .unwrap();

    let smem = unsafe {
        core::slice::from_raw_parts_mut(
            saq.get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCommand>(),
        )
    };
    const C_STRIDE: usize = size_of::<CommonCompletion>();
    const S_STRIDE: usize = size_of::<CommonCommand>();
    let mut sq = nvme::queue::SubmissionQueue::<S_STRIDE>::new(smem, 32).unwrap();

    let cmem = unsafe {
        core::slice::from_raw_parts_mut(
            caq.get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCompletion>(),
        )
    };
    let cq = nvme::queue::CompletionQueue::<C_STRIDE>::new(cmem, 32).unwrap();

    let int = ctrl.device_ctrl.events().allocate_interrupt().unwrap();
    let data = [0; S_STRIDE];
    let tail = sq.submit(&data).unwrap();

    println!("head: {}", tail);

    let saq_bell = unsafe { bar.get_mmio_offset::<VolatileCell<u32>>(0x1000) };
    let caq_bell = unsafe {
        bar.get_mmio_offset::<u32>(0x1000 + 1 * reg.capabilities.get().doorbell_stride_bytes())
    };
    saq_bell.set(tail as u32);

    let v = twizzler_async::run(async { int.next().await });

    println!("{:?}", v);
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

                    let mut ctrl = Arc::new(NvmeController {
                        requester: RwLock::new(Vec::new()),
                        device_ctrl: DeviceController::new_from_device(child),
                    });
                    init_controller(&mut ctrl);
                    return;
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
