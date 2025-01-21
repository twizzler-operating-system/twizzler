use std::{
    mem::size_of,
    sync::{Arc, Mutex, OnceLock, RwLock},
};

use nvme::{
    admin::{CreateIOCompletionQueue, CreateIOSubmissionQueue},
    ds::{
        controller::properties::config::ControllerConfig,
        identify::controller::IdentifyControllerDataStructure,
        namespace::{NamespaceId, NamespaceList},
        queue::{
            comentry::CommonCompletion, subentry::CommonCommand, CommandId, QueueId, QueuePriority,
        },
    },
    hosted::memory::{PhysicalPageCollection, PrpMode},
    nvm::{ReadDword13, WriteDword13},
};
use twizzler_async::Task;
use twizzler_driver::{
    dma::{DmaOptions, DmaPool, DMA_PAGE_SIZE},
    request::{Requester, SubmitRequest, SubmitSummaryWithResponses},
    DeviceController,
};
use volatile::map_field;

use super::{dma::NvmeDmaRegion, requester::NvmeRequester};
use crate::nvme::dma::NvmeDmaSliceRegion;

#[allow(dead_code)]
pub struct NvmeController {
    requester: RwLock<Vec<Requester<NvmeRequester>>>,
    admin_requester: RwLock<Option<Arc<Requester<NvmeRequester>>>>,
    int_tasks: Mutex<Vec<Task<()>>>,
    device_ctrl: DeviceController,
    dma_pool: DmaPool,
    capacity: OnceLock<usize>,
    block_size: OnceLock<usize>,
}

pub async fn init_controller(ctrl: &mut Arc<NvmeController>) {
    let bar = ctrl.device_ctrl.device().get_mmio(1).unwrap();
    let mut reg = unsafe {
        bar.get_mmio_offset_mut::<nvme::ds::controller::properties::ControllerProperties>(0)
    };
    let reg = reg.as_mut_ptr();

    let int = ctrl.device_ctrl.allocate_interrupt().unwrap();
    let config = ControllerConfig::new();
    map_field!(reg.configuration).write(config);

    while map_field!(reg.status).read().ready() {
        core::hint::spin_loop();
    }

    let aqa = nvme::ds::controller::properties::aqa::AdminQueueAttributes::new()
        .with_completion_queue_size(32 - 1)
        .with_submission_queue_size(32 - 1);
    map_field!(reg.admin_queue_attr).write(aqa);

    let saq = ctrl
        .dma_pool
        .allocate_array(32, nvme::ds::queue::subentry::CommonCommand::default())
        .unwrap();
    let caq = ctrl
        .dma_pool
        .allocate_array(32, nvme::ds::queue::comentry::CommonCompletion::default())
        .unwrap();

    let mut saq = NvmeDmaSliceRegion::new(saq);
    let mut caq = NvmeDmaSliceRegion::new(caq);

    let cpin = caq.dma_region_mut().pin().unwrap();
    let spin = saq.dma_region_mut().pin().unwrap();

    assert_eq!(cpin.len(), 1);
    assert_eq!(spin.len(), 1);

    let cpin_addr = cpin[0].addr();
    let spin_addr = spin[0].addr();

    map_field!(reg.admin_comqueue_base_addr).write(cpin_addr.into());
    map_field!(reg.admin_subqueue_base_addr).write(spin_addr.into());

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

    map_field!(reg.configuration).write(config);
    while !map_field!(reg.status).read().ready() {
        core::hint::spin_loop();
    }

    let smem = unsafe {
        core::slice::from_raw_parts_mut(
            saq.dma_region_mut().get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCommand>(),
        )
    };
    const C_STRIDE: usize = size_of::<CommonCompletion>();
    const S_STRIDE: usize = size_of::<CommonCommand>();
    let sq = nvme::queue::SubmissionQueue::new(smem, 32, S_STRIDE).unwrap();

    let cmem = unsafe {
        core::slice::from_raw_parts_mut(
            caq.dma_region_mut().get_mut().as_mut_ptr() as *mut u8,
            32 * size_of::<CommonCompletion>(),
        )
    };
    let cq = nvme::queue::CompletionQueue::new(cmem, 32, C_STRIDE).unwrap();

    let mut saq_bell = unsafe { bar.get_mmio_offset::<u32>(0x1000) };
    let mut caq_bell = unsafe {
        bar.get_mmio_offset::<u32>(
            0x1000 + 1 * map_field!(reg.capabilities).read().doorbell_stride_bytes(),
        )
    };

    let req = NvmeRequester::new(
        Mutex::new(sq),
        Mutex::new(cq),
        saq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
        caq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
        saq,
        caq,
    );
    let req = Arc::new(Requester::new(req));

    std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));

    let req2 = req.clone();
    let ctrl2 = ctrl.clone();
    twizzler_async::run(async {
        Task::spawn(async move {
            loop {
                let _i = int.next().await;
                //println!("got interrupt");
                //println!("=== admin ===");
                let resps = req2.driver().check_completions();
                req2.finish(&resps);
                for r in ctrl2.requester.read().unwrap().iter() {
                    //println!("=== i/o ===");
                    let c = r.driver().check_completions();
                    r.finish(&c);
                }
            }
        })
    })
    .detach();

    //ctrl.int_tasks.lock().unwrap().push(task);

    *ctrl.admin_requester.write().unwrap() = Some(req);

    let cqid = 1.into();
    let sqid = 1.into();

    let req = ctrl
        .create_queue_pair(cqid, sqid, QueuePriority::Medium, 32)
        .await;

    ctrl.requester.write().unwrap().push(req);
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
            capacity: OnceLock::new(),
            block_size: OnceLock::new(),
        }
    }

    async fn create_queue_pair(
        &self,
        cqid: QueueId,
        sqid: QueueId,
        priority: QueuePriority,
        queue_len: usize,
    ) -> Requester<NvmeRequester> {
        let saq = self
            .dma_pool
            .allocate_array(
                queue_len,
                nvme::ds::queue::subentry::CommonCommand::default(),
            )
            .unwrap();

        let caq = self
            .dma_pool
            .allocate_array(
                queue_len,
                nvme::ds::queue::comentry::CommonCompletion::default(),
            )
            .unwrap();

        let mut saq = NvmeDmaSliceRegion::new(saq);
        let spin = saq.dma_region_mut().pin().unwrap();
        assert_eq!(spin.len(), 1);

        let mut caq = NvmeDmaSliceRegion::new(caq);
        let cpin = caq.dma_region_mut().pin().unwrap();
        assert_eq!(cpin.len(), 1);

        let smem = unsafe {
            core::slice::from_raw_parts_mut(
                saq.dma_region_mut().get_mut().as_mut_ptr() as *mut u8,
                32 * size_of::<CommonCommand>(),
            )
        };

        const C_STRIDE: usize = size_of::<CommonCompletion>();
        const S_STRIDE: usize = size_of::<CommonCommand>();
        let sq = nvme::queue::SubmissionQueue::new(smem, queue_len.try_into().unwrap(), S_STRIDE)
            .unwrap();

        let cmem = unsafe {
            core::slice::from_raw_parts_mut(
                caq.dma_region_mut().get_mut().as_mut_ptr() as *mut u8,
                32 * size_of::<CommonCompletion>(),
            )
        };

        let cq = nvme::queue::CompletionQueue::new(cmem, queue_len.try_into().unwrap(), C_STRIDE)
            .unwrap();

        {
            // TODO: we should save these NvmeDmaRegions so they don't drop (dropping is okay, but
            // this leaks memory )
            let cmd = CreateIOCompletionQueue::new(
                CommandId::new(),
                cqid,
                (&mut caq)
                    .get_prp_list_or_buffer(PrpMode::Single, &self.dma_pool)
                    .unwrap(),
                ((queue_len - 1) as u16).into(),
                0,
                true,
            );

            let cmd: CommonCommand = cmd.into();
            let responses = self
                .admin_requester
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .submit_for_response(&mut [SubmitRequest::new(cmd)])
                .await
                .unwrap();

            match responses.await {
                SubmitSummaryWithResponses::Responses(_) => {}
                x => eprintln!("error creating completion queue {:?}", x),
            }
        }

        {
            let cmd = CreateIOSubmissionQueue::new(
                CommandId::new(),
                sqid,
                (&mut saq)
                    .get_prp_list_or_buffer(PrpMode::Single, &self.dma_pool)
                    .unwrap(),
                ((queue_len - 1) as u16).into(),
                cqid,
                priority,
            );
            let cmd: CommonCommand = cmd.into();
            let responses = self
                .admin_requester
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .submit_for_response(&mut [SubmitRequest::new(cmd)])
                .await
                .unwrap();
            match responses.await {
                SubmitSummaryWithResponses::Responses(_) => {}
                x => eprintln!("error creating submission queue {:?}", x),
            }
        }

        let bar = self.device_ctrl.device().get_mmio(1).unwrap();
        let reg = unsafe {
            bar.get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0)
        };
        let reg = reg.into_ptr();
        let bell_stride: usize = map_field!(reg.capabilities).read().doorbell_stride_bytes();
        let _ = 0;
        let mut saq_bell = unsafe {
            bar.get_mmio_offset::<u32>(0x1000 + (u16::from(sqid) as usize) * 2 * bell_stride)
        };
        let mut caq_bell = unsafe {
            bar.get_mmio_offset::<u32>(0x1000 + ((u16::from(cqid) as usize) * 2 + 1) * bell_stride)
        };

        let req = NvmeRequester::new(
            Mutex::new(sq),
            Mutex::new(cq),
            saq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
            caq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
            saq,
            caq,
        );

        Requester::new(req)
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
            (&mut ident)
                .get_dptr(
                    nvme::hosted::memory::DptrMode::Prp(PrpMode::Single),
                    &self.dma_pool,
                )
                .unwrap(),
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
            SubmitSummaryWithResponses::Responses(_resp) => {}
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
            (&mut nslist)
                .get_dptr(
                    nvme::hosted::memory::DptrMode::Prp(PrpMode::Single),
                    &self.dma_pool,
                )
                .unwrap(),
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
            SubmitSummaryWithResponses::Responses(_resp) => {}
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
            (&mut ident)
                .get_dptr(
                    nvme::hosted::memory::DptrMode::Prp(PrpMode::Single),
                    &self.dma_pool,
                )
                .unwrap(),
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
            SubmitSummaryWithResponses::Responses(_resp) => {}
            _ => panic!("got err for ident"),
        }

        ident.dma_region().with(|ident| ident.clone())
    }

    pub async fn _flash_len(&self) -> usize {
        if let Some(sz) = self.capacity.get() {
            *sz
        } else {
            self.identify_controller().await;
            let ns = self.identify_namespace().await;
            let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
            let _ = self.capacity.set(block_size * ns.capacity as usize);
            block_size * ns.capacity as usize
        }
    }

    pub async fn read_page(
        &self,
        lba_start: u64,
        out_buffer: &mut [u8],
        offset: usize,
    ) -> Result<(), ()> {
        let nr_blocks = DMA_PAGE_SIZE / self.get_lba_size().await;
        let buffer = self.dma_pool.allocate([0u8; DMA_PAGE_SIZE]).unwrap();
        let mut buffer = NvmeDmaRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.dma_pool,
            )
            .unwrap();
        let cmd = nvme::nvm::ReadCommand::new(
            CommandId::new(),
            NamespaceId::new(1u32),
            dptr,
            lba_start,
            nr_blocks as u16,
            ReadDword13::default(),
        );
        let cmd: CommonCommand = cmd.into();
        let responses = self.requester.read().unwrap()[0]
            .submit_for_response(&mut [SubmitRequest::new(cmd)])
            .await;
        match responses.unwrap().await {
            SubmitSummaryWithResponses::Responses(_) => buffer.dma_region().with(|data| {
                out_buffer.copy_from_slice(&data[offset..DMA_PAGE_SIZE]);
                Ok(())
            }),
            SubmitSummaryWithResponses::Errors(_, _r) => Err(()),
            SubmitSummaryWithResponses::Shutdown => Err(()),
        }
    }

    pub async fn write_page(
        &self,
        lba_start: u64,
        in_buffer: &[u8],
        offset: usize,
    ) -> Result<(), ()> {
        let nr_blocks = DMA_PAGE_SIZE / self.get_lba_size().await;
        let mut buffer = self.dma_pool.allocate([0u8; DMA_PAGE_SIZE]).unwrap();

        let len = in_buffer.len();
        if offset + len > DMA_PAGE_SIZE {
            panic!("cannot write past a page");
        }
        if offset != 0 || len != DMA_PAGE_SIZE {
            unsafe { self.read_page(lba_start, buffer.get_mut(), 0).await? };
        }
        buffer.with_mut(|data| data[offset..(offset + len)].copy_from_slice(in_buffer));

        let mut buffer = NvmeDmaRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.dma_pool,
            )
            .unwrap();
        let cmd = nvme::nvm::WriteCommand::new(
            CommandId::new(),
            NamespaceId::new(1u32),
            dptr,
            lba_start,
            nr_blocks as u16,
            WriteDword13::default(),
        );
        let cmd: CommonCommand = cmd.into();
        let responses = self.requester.read().unwrap()[0]
            .submit_for_response(&mut [SubmitRequest::new(cmd)])
            .await;
        match responses.unwrap().await {
            SubmitSummaryWithResponses::Responses(_) => Ok(()),
            SubmitSummaryWithResponses::Errors(_, _r) => Err(()),
            SubmitSummaryWithResponses::Shutdown => Err(()),
        }
    }

    pub async fn get_lba_size(&self) -> usize {
        if let Some(sz) = self.block_size.get() {
            *sz
        } else {
            self.identify_controller().await;
            let ns = self.identify_namespace().await;
            let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
            let _ = self.block_size.set(block_size);
            block_size
        }
    }
}
