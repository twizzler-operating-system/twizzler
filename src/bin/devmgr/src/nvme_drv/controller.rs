use std::{
    convert::TryInto,
    mem::size_of,
    sync::{Arc, Mutex},
};

use nvme::ds::{
    controller::properties::{capabilities::ControllerCap, config::ControllerConfig},
    queue::{comentry::CommonCompletion, subentry::CommonCommand, QueueId},
};
use twizzler_async::Task;
use twizzler_driver::{
    device::{Device, MmioObject},
    dma::{DmaOptions, DmaPool},
    request::{InFlightFutureWithResponses, Requester, SubmitError, SubmitRequest},
    DeviceController,
};
use volatile_cell::VolatileCell;

use super::queue::NvmeQueueDriver;

const ADMQ_LEN: usize = 32;
struct NvmeControllerInner {
    device: DeviceController,
    queues: Vec<Requester<NvmeQueueDriver>>,
    properties: MmioObject,
    capabilities: ControllerCap,
    int_handler: Option<Task<()>>,
}

pub struct NvmeController {
    inner: Mutex<NvmeControllerInner>,
}

pub type NvmeControllerRef = Arc<NvmeController>;

const TRANSPORT_PCIE_DOORBELL_OFFSET: usize = 0x1000;

impl NvmeController {
    pub async fn submit_admin_for_responses(
        &self,
        reqs: &mut [SubmitRequest<CommonCommand>],
    ) -> Result<InFlightFutureWithResponses<CommonCompletion>, SubmitError<()>> {
        let inner = self.inner.lock().unwrap();
        inner.queues[0].submit_for_response(reqs).await
    }

    pub fn new(device: Device) -> NvmeControllerRef {
        let bar = device.get_mmio(1).unwrap();
        let properties = unsafe {
            bar.get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0)
        };
        let caps = properties.capabilities.get();
        drop(properties);

        let ctrl = Arc::new(Self {
            inner: Mutex::new(NvmeControllerInner {
                device: DeviceController::new_from_device(device),
                queues: Vec::new(),
                properties: bar,
                capabilities: caps,
                int_handler: None,
            }),
        });

        ctrl.init_controller();

        ctrl
    }

    fn ring_bell(&self, num: usize, val: u32) {
        let inner = self.inner.lock().unwrap();
        let offset = num * inner.capabilities.doorbell_stride_bytes();
        let bell = unsafe {
            inner
                .properties
                .get_mmio_offset::<VolatileCell<u32>>(TRANSPORT_PCIE_DOORBELL_OFFSET + offset)
        };
        bell.set(val);
    }

    pub fn ring_completion_bell(&self, queue_id: QueueId, value: u32) {
        let qid: usize = queue_id.into();
        self.ring_bell(qid + 1, value);
    }

    pub fn ring_submission_bell(&self, queue_id: QueueId, value: u32) {
        let qid: usize = queue_id.into();
        self.ring_bell(qid, value);
    }

    fn init_controller(self: &NvmeControllerRef) {
        let mut inner = self.inner.lock().unwrap();

        let properties = unsafe {
            inner
                .properties
                .get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0)
        };

        let config = ControllerConfig::new();
        properties.configuration.set(config);

        while properties.status.get().ready() {
            core::hint::spin_loop();
        }

        let aqa = nvme::ds::controller::properties::aqa::AdminQueueAttributes::new()
            .with_completion_queue_size((ADMQ_LEN - 1).try_into().unwrap())
            .with_submission_queue_size((ADMQ_LEN - 1).try_into().unwrap());
        properties.admin_queue_attr.set(aqa);

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

        properties.admin_comqueue_base_addr.set(cpin_addr.into());
        properties.admin_subqueue_base_addr.set(spin_addr.into());

        //let css_nvm = properties.capabilities.get().supports_nvm_command_set();
        //let css_more = properties.capabilities.get().supports_more_io_command_sets();
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

        properties.configuration.set(config);
        while !properties.status.get().ready() {
            core::hint::spin_loop();
        }

        let smem = unsafe {
            core::slice::from_raw_parts_mut(
                saq.get_mut().as_mut_ptr() as *mut u8,
                ADMQ_LEN * size_of::<CommonCommand>(),
            )
        };
        const C_STRIDE: usize = size_of::<CommonCompletion>();
        const S_STRIDE: usize = size_of::<CommonCommand>();
        let sq = nvme::queue::SubmissionQueue::new(smem, 32, S_STRIDE).unwrap();

        let cmem = unsafe {
            core::slice::from_raw_parts_mut(
                caq.get_mut().as_mut_ptr() as *mut u8,
                ADMQ_LEN * size_of::<CommonCompletion>(),
            )
        };
        let cq = nvme::queue::CompletionQueue::new(cmem, 32, C_STRIDE).unwrap();
        let admin_queue_driver = NvmeQueueDriver::new(sq, cq, self.clone(), QueueId::ADMIN);

        let admin_queue = Requester::new(admin_queue_driver);

        inner.queues.push(admin_queue);

        let int = inner.device.allocate_interrupt().unwrap();
        let int_ctrl = self.clone();
        inner.int_handler = Some(Task::spawn(async move {
            loop {
                {
                    let inner = int_ctrl.inner.lock().unwrap();
                    inner.queues[0].driver().check_completions(&inner.queues[0]);
                }
                let _ = int.next().await;
            }
        }));

        //println!("{:?}", comp.1);

        //ident.0.with(|ident| {
        //    println!("{:#?}", ident);
        //       });

        /*

        let ident = dma
            .allocate(nvme::ds::identify::controller::IdentifyControllerDataStructure::default())
            .unwrap();
        let mut ident = NvmeDmaRegion::new(ident);
        let ident_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::IdentifyController,
            ident.get_dptr(false).unwrap(),
            None,
        );
        let ident_cmd: CommonCommand = ident_cmd.into();


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
                let responses = req.submit_for_response(&mut reqs).await;
                println!("requests submitted");
                let responses = responses.unwrap().await;
                println!("responses recieved {:?}", responses);
            }
        });
        twizzler_async::run(future::pending::<()>());
        */
    }
}
