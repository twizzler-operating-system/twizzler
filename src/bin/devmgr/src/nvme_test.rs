use std::{
    future,
    mem::size_of,
    sync::{Arc, Mutex, RwLock},
};

use nvme::{
    ds::{
        controller::properties::config::ControllerConfig,
        queue::{comentry::CommonCompletion, subentry::CommonCommand, CommandId},
    },
    hosted::memory::PhysicalPageCollection,
    queue::{CompletionQueue, SubmissionQueue},
};
use twizzler_abi::{
    device::{BusType, MailboxPriority},
    vcell::Volatile,
};
use twizzler_async::{block_on, Task};
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    device::events::InterruptInfo,
    dma::{DeviceSync, DmaOptions, DmaPool, DmaRegion},
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
        reqs: &mut [twizzler_driver::request::SubmitRequest<Self::Request>],
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
    let int = ctrl.device_ctrl.allocate_interrupt().unwrap();
}

async fn test2<'a>(ctrl: Arc<NvmeController>) {
    loop {
        let (mp, msg) = ctrl.device_ctrl.next_msg(MailboxPriority::Idle).await;
        println!("mailbox message: {:?} {}", mp, msg);
    }
}

struct NvmeDmaRegion<'a, T: DeviceSync>(DmaRegion<'a, T>);

impl<'a, T: DeviceSync> PhysicalPageCollection for NvmeDmaRegion<'a, T> {
    fn get_prp_list_or_buffer(&mut self) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        todo!()
    }

    fn get_dptr(&mut self, _sgl_allowed: bool) -> Option<nvme::ds::queue::subentry::Dptr> {
        let pin = self.0.pin().unwrap();
        Some(nvme::ds::queue::subentry::Dptr::Prp(
            pin[0].addr().into(),
            0,
        ))
    }
}

struct NvmeRequester {
    subq: Mutex<SubmissionQueue>,
    comq: Mutex<CompletionQueue>,
    sub_bell: *const VolatileCell<u32>,
    com_bell: *const VolatileCell<u32>,
}

unsafe impl Send for NvmeRequester {}
unsafe impl Sync for NvmeRequester {}

impl NvmeRequester {
    fn check_completions(&self) -> Vec<ResponseInfo<CommonCompletion>> {
        let mut comq = self.comq.lock().unwrap();
        let mut resps = Vec::new();
        let mut new_head = None;
        let mut new_bell = None;
        while let Some((bell, resp)) = comq.get_completion::<CommonCompletion>() {
            let id: u16 = resp.command_id().into();
            println!("got completion for {} {} {}", resp.new_sq_head(), bell, id);
            resps.push(ResponseInfo::new(resp, id as u64, false));
            new_head = Some(resp.new_sq_head());
            new_bell = Some(bell);
        }

        if let Some(head) = new_head {
            self.subq.lock().unwrap().update_head(head);
        }

        if let Some(bell) = new_bell {
            unsafe { self.com_bell.as_ref().unwrap().set(bell as u32) }
        }

        resps
    }
}

#[async_trait::async_trait]
impl RequestDriver for NvmeRequester {
    type Request = CommonCommand;

    type Response = CommonCompletion;

    type SubmitError = ();

    async fn submit(
        &self,
        reqs: &mut [SubmitRequest<Self::Request>],
    ) -> Result<(), Self::SubmitError> {
        let mut sq = self.subq.lock().unwrap();
        let mut tail = None;
        for sr in reqs.iter_mut() {
            println!("submitting {}", sr.id());
            let cid = (sr.id() as u16).into();
            sr.data_mut().set_cid(cid);
            tail = sq.submit(sr.data());
            println!("got tail: {:?}", tail);
            assert!(tail.is_some());
        }
        if let Some(tail) = tail {
            unsafe {
                self.sub_bell.as_ref().unwrap().set(tail as u32);
            }
        }
        Ok(())
    }

    fn flush(&self) {}

    const NUM_IDS: usize = 32;
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
        .with_completion_queue_size(32 - 1)
        .with_submission_queue_size(32 - 1);
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

    unsafe {
        println!("{:p} {:p}", saq.get(), caq.get());
    }
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

    let smem = unsafe {
        core::slice::from_raw_parts_mut(
            saq.get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCommand>(),
        )
    };
    const C_STRIDE: usize = size_of::<CommonCompletion>();
    const S_STRIDE: usize = size_of::<CommonCommand>();
    let mut sq = nvme::queue::SubmissionQueue::new(smem, 32, S_STRIDE).unwrap();

    let cmem = unsafe {
        core::slice::from_raw_parts_mut(
            caq.get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCompletion>(),
        )
    };
    let mut cq = nvme::queue::CompletionQueue::new(cmem, 32, C_STRIDE).unwrap();

    let int = ctrl.device_ctrl.allocate_interrupt().unwrap();
    let ident = dma
        .allocate(nvme::ds::identify::controller::IdentifyControllerDataStructure::default())
        .unwrap();
    let mut ident = NvmeDmaRegion(ident);
    let ident_cmd = nvme::admin::Identify::new(
        CommandId::new(),
        nvme::admin::IdentifyCNSValue::IdentifyController,
        ident.get_dptr(false).unwrap(),
        None,
    );
    let ident_cmd: CommonCommand = ident_cmd.into();

    let data = [0u8; S_STRIDE];
    let tail = sq.submit(&ident_cmd).unwrap();

    println!("head: {}", tail);

    let saq_bell = unsafe { bar.get_mmio_offset::<VolatileCell<u32>>(0x1000) };
    let caq_bell = unsafe {
        bar.get_mmio_offset::<VolatileCell<u32>>(
            0x1000 + 1 * reg.capabilities.get().doorbell_stride_bytes(),
        )
    };
    saq_bell.set(tail as u32);

    let v = twizzler_async::run(async { int.next().await });

    let comp: (u16, CommonCompletion) = cq.get_completion().unwrap();
    sq.update_head(comp.1.new_sq_head());

    caq_bell.set(comp.0 as u32);

    //println!("{:?}", comp.1);

    ident.0.with(|ident| {
        //    println!("{:#?}", ident);
    });

    let req = NvmeRequester {
        subq: Mutex::new(sq),
        comq: Mutex::new(cq),
        sub_bell: saq_bell as *const VolatileCell<u32>,
        com_bell: caq_bell as *const VolatileCell<u32>,
    };
    let req = Arc::new(Requester::new(req));

    let req2 = req.clone();
    let task = Task::spawn(async move {
        loop {
            let i = int.next().await;
            println!("got interrupt");
            let resps = req2.driver().check_completions();
            req2.finish(&resps);
        }
    });

    let mut reqs = [SubmitRequest::new(ident_cmd)];
    let submitter = Task::spawn(async move {
        loop {
            let responses = req.submit_for_response(&mut reqs).await.unwrap().await;
            println!("{:?}", responses);
        }
    });
    twizzler_async::run(future::pending::<()>());
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
