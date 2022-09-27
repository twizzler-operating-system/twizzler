use std::{convert::TryInto, mem::size_of, sync::Arc};

use async_rwlock::RwLock;
use nvme::{
    ds::{
        controller::properties::{capabilities::ControllerCap, config::ControllerConfig},
        identify::controller::IdentifyControllerDataStructure,
        queue::{comentry::CommonCompletion, subentry::CommonCommand, CommandId, QueueId},
    },
    hosted::memory::PhysicalPageCollection,
};
use twizzler_async::Task;
use twizzler_driver::{
    device::{Device, MmioObject},
    dma::{DmaOptions, DmaPool, DmaRegion},
    request::{
        InFlightFutureWithResponses, Requester, SubmitError, SubmitRequest,
        SubmitSummaryWithResponses,
    },
    DeviceController,
};
use volatile_cell::VolatileCell;

use crate::nvme_drv::queue::NvmeQueue;

use super::{memory::NvmeDmaRegion, queue::NvmeQueueDriver};

const ADMQ_LEN: usize = 32;
struct NvmeControllerInner {
    device: DeviceController,
    queues: Vec<NvmeQueue>,
    properties: MmioObject,
    capabilities: ControllerCap,
    int_handler: Option<Task<()>>,
    queue_id_max: u16,
    queue_id_free: Vec<u16>,
    dma: DmaPool,
}

pub struct NvmeController {
    inner: RwLock<NvmeControllerInner>,
}

pub type NvmeControllerRef = Arc<NvmeController>;

const TRANSPORT_PCIE_DOORBELL_OFFSET: usize = 0x1000;

#[derive(Debug, Clone, Copy)]
pub enum RequestError {
    SubmitError(SubmitError<()>),
    ErrResponse(CommonCompletion),
}

impl NvmeControllerInner {
    fn allocate_queue_id(&mut self) -> QueueId {
        if let Some(id) = self.queue_id_free.pop() {
            id
        } else {
            let next = self.queue_id_max;
            self.queue_id_max += 1;
            next
        }
        .into()
    }

    fn free_queue_id(&mut self, id: QueueId) {
        self.queue_id_free.push(id.into())
    }

    fn create_queue(
        &mut self,
        sqlen: usize,
        cqlen: usize,
        admin: bool,
        outer: &NvmeControllerRef,
    ) -> QueueId {
        const C_STRIDE: usize = size_of::<CommonCompletion>();
        const S_STRIDE: usize = size_of::<CommonCommand>();

        if !self.queues.is_empty() && admin {
            panic!("admin queue already created");
        }

        // Create a DMA region for the submission queue.
        let mut sq_reg = self
            .dma
            .allocate_array(sqlen, nvme::ds::queue::subentry::CommonCommand::default())
            .unwrap();

        // Create a DMA region for the completion queue.
        let mut cq_reg = self
            .dma
            .allocate_array(
                cqlen,
                nvme::ds::queue::comentry::CommonCompletion::default(),
            )
            .unwrap();

        // Create a slice from the DMA region and pass it to the nvme library to make a handle.
        let smem = unsafe {
            core::slice::from_raw_parts_mut(
                sq_reg.get_mut().as_mut_ptr() as *mut u8,
                sqlen * size_of::<CommonCommand>(),
            )
        };
        let sq =
            nvme::queue::SubmissionQueue::new(smem, sqlen.try_into().unwrap(), S_STRIDE).unwrap();

        // Same for completion queue memory.
        let cmem = unsafe {
            core::slice::from_raw_parts_mut(
                cq_reg.get_mut().as_mut_ptr() as *mut u8,
                cqlen * size_of::<CommonCompletion>(),
            )
        };
        let cq =
            nvme::queue::CompletionQueue::new(cmem, cqlen.try_into().unwrap(), C_STRIDE).unwrap();

        // If we're creating the admin queue, then use the hardcoded ID, otherwise allocate one.
        let id = if admin {
            QueueId::ADMIN
        } else {
            self.allocate_queue_id()
        };

        // Create the requester framework and the new NvmeQueue.
        let queue_driver = NvmeQueueDriver::new(sq, cq, outer.clone(), id);
        let queue_req = Requester::new(queue_driver);
        let queue = NvmeQueue::new(queue_req, sq_reg, cq_reg);

        // Push this onto our list.
        self.queues.push(queue);
        id
    }
}

impl NvmeController {
    pub async fn submit_admin_for_responses(
        &self,
        reqs: &mut [SubmitRequest<CommonCommand>],
    ) -> Result<InFlightFutureWithResponses<CommonCompletion>, SubmitError<()>> {
        let inner = self.inner.read().await;
        inner.queues[0].requester().submit_for_response(reqs).await
    }

    pub fn new(device: Device) -> NvmeControllerRef {
        const PCIE_NVME_BAR_MMIO: u8 = 1;
        let bar = device.get_mmio(PCIE_NVME_BAR_MMIO).unwrap();
        let properties = unsafe {
            bar.get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0)
        };
        let caps = properties.capabilities.get();
        drop(properties);
        let dma = DmaPool::new(
            DmaPool::default_spec(),
            twizzler_driver::dma::Access::BiDirectional,
            DmaOptions::empty(),
        );
        let ctrl = Arc::new(Self {
            inner: RwLock::new(NvmeControllerInner {
                dma,
                device: DeviceController::new_from_device(device),
                queues: Vec::new(),
                properties: bar,
                capabilities: caps,
                int_handler: None,
                queue_id_free: vec![],
                queue_id_max: 1u16.into(), // Admin queue is ID 0, and is reserved
            }),
        });

        ctrl
    }

    async fn ring_bell(&self, num: usize, val: u32) {
        let inner = self.inner.read().await;
        let offset = num * inner.capabilities.doorbell_stride_bytes();
        let bell = unsafe {
            inner
                .properties
                .get_mmio_offset::<VolatileCell<u32>>(TRANSPORT_PCIE_DOORBELL_OFFSET + offset)
        };
        bell.set(val);
    }

    pub async fn ring_completion_bell(&self, queue_id: QueueId, value: u32) {
        let qid: usize = queue_id.into();
        self.ring_bell(qid + 1, value).await;
    }

    pub async fn ring_submission_bell(&self, queue_id: QueueId, value: u32) {
        let qid: usize = queue_id.into();
        self.ring_bell(qid, value).await;
    }

    pub async fn free_queue_id(&self, id: QueueId) {
        self.inner.write().await.free_queue_id(id)
    }

    pub async fn identify_controller(
        self: &NvmeControllerRef,
    ) -> Result<DmaRegion<IdentifyControllerDataStructure>, RequestError> {
        let inner = self.inner.read().await;
        let ident = inner
            .dma
            .allocate(IdentifyControllerDataStructure::default())
            .unwrap();

        let mut ident = NvmeDmaRegion::new(ident, self);
        let ident_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::IdentifyController,
            ident.get_dptr(false).unwrap(),
            None,
        );
        let ident_cmd: CommonCommand = ident_cmd.into();

        let mut reqs = [SubmitRequest::new(ident_cmd)];
        let result = inner.queues[0]
            .requester()
            .submit_for_response(&mut reqs)
            .await
            .map_err(|e| RequestError::SubmitError(e))?
            .await;

        let result = match result {
            SubmitSummaryWithResponses::Responses(_) => Ok(ident.into_dma_reg()),
            SubmitSummaryWithResponses::Errors(_, v) => Err(RequestError::ErrResponse(v[0])),
            SubmitSummaryWithResponses::Shutdown => {
                Err(RequestError::SubmitError(SubmitError::IsShutdown))
            }
        };

        result
    }

    pub async fn init_controller(self: &NvmeControllerRef) {
        let mut inner = self.inner.write().await;

        // Start by creating the admin queue pair in memory.
        inner.create_queue(ADMQ_LEN, ADMQ_LEN, true, self);
        let admin_queue = &mut inner.queues[0];

        // Grab the physical addresses of the admin completion and submission queues.
        let cpin = admin_queue.completion_dma_region().pin().unwrap();
        assert_eq!(cpin.len(), 1);
        let cpin_addr = cpin[0].addr();

        let spin = admin_queue.submission_dma_region().pin().unwrap();
        assert_eq!(spin.len(), 1);
        let spin_addr = spin[0].addr();

        let properties = unsafe {
            inner
                .properties
                .get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0)
        };

        // Reset the controller configuration.
        let config = ControllerConfig::new();
        properties.configuration.set(config);

        // Wait until the ready status clears to indicate reset.
        while properties.status.get().ready() {
            core::hint::spin_loop();
        }

        // Fill out the AQA register with admin queue length (zero-based).
        let aqa = nvme::ds::controller::properties::aqa::AdminQueueAttributes::new()
            .with_completion_queue_size((ADMQ_LEN - 1).try_into().unwrap())
            .with_submission_queue_size((ADMQ_LEN - 1).try_into().unwrap());
        properties.admin_queue_attr.set(aqa);

        // ... and set them in the controller properties.
        properties.admin_comqueue_base_addr.set(cpin_addr.into());
        properties.admin_subqueue_base_addr.set(spin_addr.into());

        //let css_nvm = properties.capabilities.get().supports_nvm_command_set();
        //let css_more = properties.capabilities.get().supports_more_io_command_sets();
        // TODO: check bit 7 of css.

        // Setup other config properties.
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

        // Enable the controller and wait for it to indicate ready.
        properties.configuration.set(config);
        while !properties.status.get().ready() {
            core::hint::spin_loop();
        }

        // Allocate an interrupt for our device.
        let int = inner.device.allocate_interrupt().unwrap();
        let int_ctrl = self.clone();
        inner.int_handler = Some(Task::spawn(async move {
            // Interrupt handler task. Will only get run once we actually start the async system via run().
            loop {
                {
                    let inner = int_ctrl.inner.read().await;
                    // Check all queues for completions. NOTE: In future, we can allocate different
                    // interrupts for different queues.
                    for queue in &inner.queues {
                        queue
                            .requester()
                            .driver()
                            .check_completions(&queue.requester())
                            .await;
                    }
                }
                let _ = int.next().await;
            }
        }));
    }
}
