use core::panic;
use std::{
    mem::size_of,
    sync::{Arc, Mutex, RwLock},
};

use nvme::{
    ds::{
        controller::properties::config::ControllerConfig,
        identify::controller::IdentifyControllerDataStructure,
        namespace::{NamespaceId, NamespaceList},
        queue::{comentry::CommonCompletion, subentry::CommonCommand, CommandId},
    },
    hosted::memory::PhysicalPageCollection,
    queue::{CompletionQueue, SubmissionQueue},
};
use twizzler_abi::device::BusType;
use twizzler_async::Task;
use twizzler_driver::{
    bus::pcie::PcieDeviceInfo,
    dma::{DeviceSync, DmaOptions, DmaPool, DmaRegion},
    request::{RequestDriver, Requester, ResponseInfo, SubmitRequest},
    DeviceController,
};
use volatile_cell::VolatileCell;

pub struct NvmeController {
    requester: RwLock<Vec<Requester<NvmeRequester>>>,
    admin_requester: RwLock<Option<Arc<Requester<NvmeRequester>>>>,
    int_tasks: Mutex<Vec<Task<()>>>,
    device_ctrl: DeviceController,
    dma_pool: DmaPool,
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
            //println!("got completion for {} {} {}", resp.new_sq_head(), bell, id);
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
            let cid = (sr.id() as u16).into();
            sr.data_mut().set_cid(cid);
            tail = sq.submit(sr.data());
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
            let _i = int.next().await;
            println!("got interrupt");
            let resps = req2.driver().check_completions();
            req2.finish(&resps);
        }
    });
    ctrl.int_tasks.lock().unwrap().push(task);

    *ctrl.admin_requester.write().unwrap() = Some(req);
}

pub fn init_nvme() -> Arc<NvmeController> {
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
                        int_tasks: Mutex::default(),
                        dma_pool: DmaPool::new(
                            DmaPool::default_spec(),
                            twizzler_driver::dma::Access::BiDirectional,
                            DmaOptions::empty(),
                        ),
                        requester: RwLock::new(Vec::new()),
                        device_ctrl: DeviceController::new_from_device(child),
                        admin_requester: RwLock::new(None),
                    });
                    init_controller(&mut ctrl);
                    return ctrl;
                }
            }
        }
    }
    panic!("no nvme controller found");
}

impl NvmeController {
    pub async fn identify_controller(&self) -> IdentifyControllerDataStructure {
        let ident = self
            .dma_pool
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

        ident.0.with(|ident| ident.clone())
    }

    pub async fn identify_namespace(
        &self,
    ) -> nvme::ds::identify::namespace::IdentifyNamespaceDataStructure {
        let nslist = self.dma_pool.allocate([0u8; 4096]).unwrap();
        let mut nslist = NvmeDmaRegion(nslist);
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

        nslist.0.with(|nslist| {
            let lslist = NamespaceList::new(nslist);
            for _id in lslist.into_iter() {
                // TODO: do something with IDs
            }
        });

        let ident = self
            .dma_pool
            .allocate(nvme::ds::identify::namespace::IdentifyNamespaceDataStructure::default())
            .unwrap();
        let mut ident = NvmeDmaRegion(ident);
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

        ident.0.with(|ident| ident.clone())
    }

    pub async fn flash_len(&self) -> usize {
        self.identify_controller().await;
        let ns = self.identify_namespace().await;
        let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
        block_size * ns.capacity as usize
    }
}
