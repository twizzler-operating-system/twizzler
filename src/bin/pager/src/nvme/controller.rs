use std::{
    mem::size_of,
    sync::{Arc, Mutex, RwLock},
};

use nvme::ds::{
    controller::properties::config::ControllerConfig,
    identify::controller::IdentifyControllerDataStructure,
    namespace::{NamespaceId, NamespaceList},
    queue::{comentry::CommonCompletion, subentry::CommonCommand, CommandId},
};
use nvme::hosted::memory::PhysicalPageCollection;
use twizzler_async::Task;
use twizzler_driver::{
    dma::{DmaOptions, DmaPool},
    request::{Requester, SubmitRequest},
    DeviceController,
};
use volatile_cell::VolatileCell;

use crate::store::BLOCK_SIZE;

use super::{dma::NvmeDmaRegion, requester::NvmeRequester};

pub struct NvmeController {
    requester: RwLock<Vec<Requester<NvmeRequester>>>,
    admin_requester: RwLock<Option<Arc<Requester<NvmeRequester>>>>,
    int_tasks: Mutex<Vec<Task<()>>>,
    device_ctrl: DeviceController,
    dma_pool: DmaPool,
}

pub fn init_controller(ctrl: &mut Arc<NvmeController>) {
    let bar = ctrl.device_ctrl.device().get_mmio(1).unwrap();
    let reg =
        unsafe { bar.get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0) };

    let int = ctrl.device_ctrl.allocate_interrupt().unwrap();
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

    let mut saq = ctrl
        .dma_pool
        .allocate_array(32, nvme::ds::queue::subentry::CommonCommand::default())
        .unwrap();
    let mut caq = ctrl
        .dma_pool
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
                .ilog2()
                .try_into()
                .unwrap(),
        )
        .with_io_submission_queue_entry_size(
            size_of::<CommonCommand>()
                .next_power_of_two()
                .ilog2()
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
    let sq = nvme::queue::SubmissionQueue::new(smem, 32, S_STRIDE).unwrap();

    let cmem = unsafe {
        core::slice::from_raw_parts_mut(
            caq.get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCompletion>(),
        )
    };
    let cq = nvme::queue::CompletionQueue::new(cmem, 32, C_STRIDE).unwrap();

    let saq_bell = unsafe { bar.get_mmio_offset::<VolatileCell<u32>>(0x1000) };
    let caq_bell = unsafe {
        bar.get_mmio_offset::<VolatileCell<u32>>(
            0x1000 + 1 * reg.capabilities.get().doorbell_stride_bytes(),
        )
    };

    let req = NvmeRequester::new(
        Mutex::new(sq),
        Mutex::new(cq),
        saq_bell as *const VolatileCell<u32>,
        caq_bell as *const VolatileCell<u32>,
    );
    let req = Arc::new(Requester::new(req));

    let req2 = req.clone();
    let task = Task::spawn(async move {
        loop {
            let _i = int.next().await;
            println!("got interrupt");
            let resps = req2.driver().check_completions();
            req2.finish(&resps);
        }
    });
    ctrl.int_tasks.lock().unwrap().push(task);

    *ctrl.admin_requester.write().unwrap() = Some(req);
}

impl NvmeController {
    pub fn new(device_ctrl: DeviceController) -> Self {
        Self {
            requester: Default::default(),
            admin_requester: Default::default(),
            int_tasks: Default::default(),
            device_ctrl,
            dma_pool: DmaPool::new(
                DmaPool::default_spec(),
                twizzler_driver::dma::Access::BiDirectional,
                DmaOptions::empty(),
            ),
        }
    }

    pub async fn identify_controller(&self) -> IdentifyControllerDataStructure {
        let ident = self
            .dma_pool
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
        let responses = self
            .admin_requester
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .submit_for_response(&mut [SubmitRequest::new(ident_cmd)])
            .await;
        let responses = responses.unwrap().await;
        match responses {
            twizzler_driver::request::SubmitSummaryWithResponses::Responses(_resp) => {}
            _ => panic!("got err for ident"),
        }

        ident.dma_region().with(|ident| ident.clone())
    }

    pub async fn identify_namespace(
        &self,
    ) -> nvme::ds::identify::namespace::IdentifyNamespaceDataStructure {
        let nslist = self.dma_pool.allocate([0u8; 4096]).unwrap();
        let mut nslist = NvmeDmaRegion::new(nslist);
        let nslist_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::ActiveNamespaceIdList(NamespaceId::default()),
            nslist.get_dptr(false).unwrap(),
            None,
        );
        let nslist_cmd: CommonCommand = nslist_cmd.into();
        let responses = self
            .admin_requester
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .submit_for_response(&mut [SubmitRequest::new(nslist_cmd)])
            .await;
        let responses = responses.unwrap().await;
        match responses {
            twizzler_driver::request::SubmitSummaryWithResponses::Responses(_resp) => {}
            _ => panic!("got err for ident"),
        }

        nslist.dma_region().with(|nslist| {
            let lslist = NamespaceList::new(nslist);
            for _id in lslist.into_iter() {
                // TODO: do something with IDs
            }
        });

        let ident = self
            .dma_pool
            .allocate(nvme::ds::identify::namespace::IdentifyNamespaceDataStructure::default())
            .unwrap();
        let mut ident = NvmeDmaRegion::new(ident);
        let ident_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::IdentifyNamespace(NamespaceId::new(1u32)),
            ident.get_dptr(false).unwrap(),
            None,
        );
        let ident_cmd: CommonCommand = ident_cmd.into();
        let responses = self
            .admin_requester
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .submit_for_response(&mut [SubmitRequest::new(ident_cmd)])
            .await;
        let responses = responses.unwrap().await;
        match responses {
            twizzler_driver::request::SubmitSummaryWithResponses::Responses(_resp) => {}
            _ => panic!("got err for ident"),
        }

        ident.dma_region().with(|ident| ident.clone())
    }

    pub async fn flash_len(&self) -> usize {
        self.identify_controller().await;
        let ns = self.identify_namespace().await;
        let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
        block_size * ns.capacity as usize
    }

    pub async fn _read_data(&self, _block: u64) {
        let buffer = self.dma_pool.allocate([0u8; BLOCK_SIZE]).unwrap();
        let _buffer = NvmeDmaRegion::new(buffer);
    }
}
