use std::{
    future::Future,
    io::ErrorKind,
    mem::size_of,
    sync::{Arc, OnceLock},
    thread::JoinHandle,
    time::Instant,
};

use async_io::Async;
use nvme::{
    admin::{CreateIOCompletionQueue, CreateIOSubmissionQueue},
    ds::{
        controller::properties::config::ControllerConfig,
        identify::{
            controller::IdentifyControllerDataStructure, namespace::IdentifyNamespaceDataStructure,
        },
        namespace::NamespaceId,
        queue::{
            comentry::CommonCompletion,
            subentry::{CommonCommand, Dptr},
            CommandId, QueueId, QueuePriority,
        },
    },
    hosted::memory::{PhysicalPageCollection, PrpMode},
    nvm::{ReadDword13, WriteDword13},
};
use twizzler_driver::{
    device::Device,
    dma::{DmaOptions, DmaPool, PhysInfo, DMA_PAGE_SIZE},
};
use volatile::map_field;

use super::{
    dma::{CachedDmaPool, NvmeDmaRegion},
    requester::{InflightRequest, NvmeRequester},
};
use crate::nvme::dma::NvmeDmaSliceRegion;

struct NvmeControllerInner {
    data_requester: NvmeRequester,
    admin_requester: NvmeRequester,
    device: Device,
    dma_pool: Arc<CachedDmaPool>,
}

pub struct NvmeController {
    inner: Arc<NvmeControllerInner>,
    capacity: OnceLock<usize>,
    block_size: OnceLock<usize>,
    int_thr: OnceLock<JoinHandle<()>>,
}

const ADMIN_QUEUE_LEN: u16 = 32;
const DATA_QUEUE_ID: u16 = 1;
const DATA_QUEUE_LEN: u16 = 32;

fn init_controller(mut device: Device, dma_pool: DmaPool) -> std::io::Result<NvmeController> {
    let dma_pool = Arc::new(CachedDmaPool::new(dma_pool));
    let bar = device.get_mmio(1).unwrap();
    let mut reg = unsafe {
        bar.get_mmio_offset_mut::<nvme::ds::controller::properties::ControllerProperties>(0)
    };
    let reg = reg.as_mut_ptr();

    let _int = device
        .allocate_interrupt(0)
        .expect("failed to allocate interrupt");
    let config = ControllerConfig::new();
    map_field!(reg.configuration).write(config);

    while map_field!(reg.status).read().ready() {
        core::hint::spin_loop();
    }

    let aqa = nvme::ds::controller::properties::aqa::AdminQueueAttributes::new()
        .with_completion_queue_size(ADMIN_QUEUE_LEN - 1)
        .with_submission_queue_size(ADMIN_QUEUE_LEN - 1);
    map_field!(reg.admin_queue_attr).write(aqa);

    let saq = dma_pool
        .dma
        .allocate_array(
            ADMIN_QUEUE_LEN as usize,
            nvme::ds::queue::subentry::CommonCommand::default(),
        )
        .unwrap();
    let caq = dma_pool
        .dma
        .allocate_array(
            ADMIN_QUEUE_LEN as usize,
            nvme::ds::queue::comentry::CommonCompletion::default(),
        )
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
            ADMIN_QUEUE_LEN as usize * size_of::<CommonCommand>(),
        )
    };
    const C_STRIDE: usize = size_of::<CommonCompletion>();
    const S_STRIDE: usize = size_of::<CommonCommand>();
    let sq = nvme::queue::SubmissionQueue::new(smem, 32, S_STRIDE).unwrap();

    let cmem = unsafe {
        core::slice::from_raw_parts_mut(
            caq.dma_region_mut().get_mut().as_mut_ptr() as *mut u8,
            ADMIN_QUEUE_LEN as usize * size_of::<CommonCompletion>(),
        )
    };
    let cq = nvme::queue::CompletionQueue::new(cmem, ADMIN_QUEUE_LEN, C_STRIDE).unwrap();

    let mut saq_bell = unsafe { bar.get_mmio_offset::<u32>(0x1000) };
    let mut caq_bell = unsafe {
        bar.get_mmio_offset::<u32>(
            0x1000 + 1 * map_field!(reg.capabilities).read().doorbell_stride_bytes(),
        )
    };

    let mut admin_requester = NvmeRequester::new(
        sq,
        cq,
        saq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
        caq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
        bar,
        saq,
        caq,
    );

    let cqid = DATA_QUEUE_ID.into();
    let sqid = DATA_QUEUE_ID.into();

    let req = NvmeController::create_queue_pair(
        &mut admin_requester,
        &dma_pool,
        &mut device,
        cqid,
        sqid,
        QueuePriority::Medium,
        DATA_QUEUE_LEN as usize,
    )?;

    Ok(NvmeController {
        inner: Arc::new(NvmeControllerInner {
            data_requester: req,
            admin_requester,
            device,
            dma_pool,
        }),
        capacity: OnceLock::new(),
        block_size: OnceLock::new(),
        int_thr: OnceLock::new(),
    })
}

fn interrupt_thread_main(inner: &NvmeControllerInner, inum: usize) {
    loop {
        let more = inner.device.repr().check_for_interrupt(inum).is_some();

        let more_a = inner.admin_requester.check_completions();
        let more_d = inner.data_requester.check_completions();

        if !more && !more_a && !more_d {
            inner.device.repr().wait_for_interrupt(inum, None);
        }
    }
}

#[allow(dead_code)]
impl NvmeController {
    pub fn new(device: Device) -> std::io::Result<Self> {
        let dma_pool = DmaPool::new(
            DmaPool::default_spec(),
            twizzler_driver::dma::Access::BiDirectional,
            DmaOptions::empty(),
        );

        let ctrl = init_controller(device, dma_pool)?;
        let inner = ctrl.inner.clone();
        ctrl.int_thr
            .set(
                std::thread::Builder::new()
                    .name("nvme-int-0".to_string())
                    .spawn(move || {
                        interrupt_thread_main(&inner, 0);
                    })
                    .unwrap(),
            )
            .unwrap();
        Ok(ctrl)
    }

    fn create_queue_pair(
        admin_requester: &mut NvmeRequester,
        dma_pool: &Arc<CachedDmaPool>,
        device: &mut Device,
        cqid: QueueId,
        sqid: QueueId,
        priority: QueuePriority,
        queue_len: usize,
    ) -> std::io::Result<NvmeRequester> {
        let saq = dma_pool
            .dma
            .allocate_array(
                queue_len,
                nvme::ds::queue::subentry::CommonCommand::default(),
            )
            .unwrap();

        let caq = dma_pool
            .dma
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
                queue_len * size_of::<CommonCommand>(),
            )
        };

        const C_STRIDE: usize = size_of::<CommonCompletion>();
        const S_STRIDE: usize = size_of::<CommonCommand>();
        let sq = nvme::queue::SubmissionQueue::new(smem, queue_len.try_into().unwrap(), S_STRIDE)
            .unwrap();

        let cmem = unsafe {
            core::slice::from_raw_parts_mut(
                caq.dma_region_mut().get_mut().as_mut_ptr() as *mut u8,
                queue_len * size_of::<CommonCompletion>(),
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
                    .get_prp_list_or_buffer(PrpMode::Single, dma_pool)
                    .unwrap(),
                ((queue_len - 1) as u16).into(),
                0,
                true,
            );

            let cmd: CommonCommand = cmd.into();
            let inflight = admin_requester.submit(cmd).unwrap();
            loop {
                if let Some(resp) = admin_requester.get_completion() {
                    if resp.status().is_error() {
                        return Err(ErrorKind::Other.into());
                    }
                    if inflight.id != resp.command_id().into() {
                        tracing::error!("got other command ID for queue create command");
                    }
                    break;
                }
            }
        }

        {
            let cmd = CreateIOSubmissionQueue::new(
                CommandId::new(),
                sqid,
                (&mut saq)
                    .get_prp_list_or_buffer(PrpMode::Single, dma_pool)
                    .unwrap(),
                ((queue_len - 1) as u16).into(),
                cqid,
                priority,
            );
            let cmd: CommonCommand = cmd.into();
            let cmd: CommonCommand = cmd.into();
            let inflight = admin_requester.submit(cmd).unwrap();
            loop {
                if let Some(resp) = admin_requester.get_completion() {
                    if resp.status().is_error() {
                        return Err(ErrorKind::Other.into());
                    }
                    if inflight.id != resp.command_id().into() {
                        tracing::error!("got other command ID for queue create command");
                    }
                    break;
                }
            }
        }

        let bar = device.get_mmio(1).unwrap();
        let reg = unsafe {
            bar.get_mmio_offset::<nvme::ds::controller::properties::ControllerProperties>(0)
        };
        let reg = reg.into_ptr();
        let bell_stride: usize = map_field!(reg.capabilities).read().doorbell_stride_bytes();
        let mut saq_bell = unsafe {
            bar.get_mmio_offset::<u32>(0x1000 + (u16::from(sqid) as usize) * 2 * bell_stride)
        };
        let mut caq_bell = unsafe {
            bar.get_mmio_offset::<u32>(0x1000 + ((u16::from(cqid) as usize) * 2 + 1) * bell_stride)
        };

        let req = NvmeRequester::new(
            sq,
            cq,
            saq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
            caq_bell.as_mut_ptr().as_raw_ptr().as_ptr(),
            bar,
            saq,
            caq,
        );

        Ok(req)
    }

    pub fn send_identify_controller(
        &self,
    ) -> Option<(
        InflightRequest<'_>,
        NvmeDmaRegion<IdentifyControllerDataStructure>,
    )> {
        let ident = self
            .inner
            .dma_pool
            .dma
            .allocate(nvme::ds::identify::controller::IdentifyControllerDataStructure::default())
            .unwrap();
        let mut ident = NvmeDmaRegion::new(ident);
        let ident_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::IdentifyController,
            (&mut ident)
                .get_dptr(
                    nvme::hosted::memory::DptrMode::Prp(PrpMode::Single),
                    &self.inner.dma_pool,
                )
                .unwrap(),
            None,
        );
        let ident_cmd: CommonCommand = ident_cmd.into();
        let inflight = self.inner.admin_requester.submit(ident_cmd)?;

        Some((inflight, ident))
    }

    pub fn send_list_namespaces(
        &self,
    ) -> Option<(InflightRequest<'_>, NvmeDmaRegion<[u8; DMA_PAGE_SIZE]>)> {
        let nslist = self
            .inner
            .dma_pool
            .dma
            .allocate([0u8; DMA_PAGE_SIZE])
            .unwrap();
        let mut nslist = NvmeDmaRegion::new(nslist);
        let nslist_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::ActiveNamespaceIdList(NamespaceId::default()),
            (&mut nslist)
                .get_dptr(
                    nvme::hosted::memory::DptrMode::Prp(PrpMode::Single),
                    &self.inner.dma_pool,
                )
                .unwrap(),
            None,
        );
        let nslist_cmd: CommonCommand = nslist_cmd.into();
        let inflight = self.inner.admin_requester.submit(nslist_cmd)?;
        Some((inflight, nslist))
    }

    pub fn send_identify_namespace(
        &self,
        nsid: NamespaceId,
    ) -> Option<(
        InflightRequest<'_>,
        NvmeDmaRegion<IdentifyNamespaceDataStructure>,
    )> {
        let ident = self
            .inner
            .dma_pool
            .dma
            .allocate(nvme::ds::identify::namespace::IdentifyNamespaceDataStructure::default())
            .unwrap();
        let mut ident = NvmeDmaRegion::new(ident);
        let ident_cmd = nvme::admin::Identify::new(
            CommandId::new(),
            nvme::admin::IdentifyCNSValue::IdentifyNamespace(nsid),
            (&mut ident)
                .get_dptr(
                    nvme::hosted::memory::DptrMode::Prp(PrpMode::Single),
                    &self.inner.dma_pool,
                )
                .unwrap(),
            None,
        );
        let ident_cmd: CommonCommand = ident_cmd.into();
        let inflight = self.inner.admin_requester.submit(ident_cmd)?;
        Some((inflight, ident))
    }

    pub async fn identify_controller(&self) -> std::io::Result<IdentifyControllerDataStructure> {
        // TODO: queue full
        let (inflight, ident_dma) = self.send_identify_controller().unwrap();
        let asif = Async::new(inflight)?;
        let cc = asif
            .read_with(|inflight| {
                while let Some(_) = inflight.req.get_completion() {}
                inflight.poll()
            })
            .await?;
        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        Ok(ident_dma.dma_region().with(|ident| ident.clone()))
    }

    pub async fn identify_namespace(
        &self,
        nsid: NamespaceId,
    ) -> std::io::Result<IdentifyNamespaceDataStructure> {
        // TODO: queue full
        let (inflight, ident_dma) = self.send_identify_namespace(nsid).unwrap();
        let asif = Async::new(inflight)?;
        let cc = asif
            .read_with(|inflight| {
                while let Some(_) = inflight.req.get_completion() {}
                inflight.poll()
            })
            .await?;
        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        Ok(ident_dma.dma_region().with(|ident| ident.clone()))
    }

    pub async fn flash_len(&self) -> usize {
        if let Some(sz) = self.capacity.get() {
            *sz
        } else {
            let ns = self
                .identify_namespace(NamespaceId::new(1u32))
                .await
                .unwrap();
            let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
            let _ = self.capacity.set(block_size * ns.capacity as usize);
            block_size * ns.capacity as usize
        }
    }

    pub async fn get_lba_size(&self) -> usize {
        if let Some(sz) = self.block_size.get() {
            *sz
        } else {
            let ns = self
                .identify_namespace(NamespaceId::new(1u32))
                .await
                .unwrap();
            let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
            let _ = self.block_size.set(block_size);
            block_size
        }
    }

    pub fn blocking_get_lba_size(&self) -> usize {
        if let Some(sz) = self.block_size.get() {
            *sz
        } else {
            let (inflight, dma) = self
                .send_identify_namespace(NamespaceId::new(1u32))
                .unwrap();
            let cc = inflight.wait().unwrap();
            if cc.status().is_error() {
                panic!("error on ident ns")
            }
            let ns = dma.dma_region().with(|ident| ident.clone());
            let block_size = ns.lba_formats()[ns.formatted_lba_size.index()].data_size();
            let _ = self.block_size.set(block_size);
            block_size
        }
    }

    pub fn send_read_page(
        &self,
        lba_start: u64,
        dptr: Dptr,
        nr_blocks_per_page: usize,
        block: bool,
    ) -> Option<InflightRequest<'_>> {
        let cmd = nvme::nvm::ReadCommand::new(
            CommandId::new(),
            NamespaceId::new(1u32),
            dptr,
            lba_start,
            nr_blocks_per_page as u16,
            ReadDword13::default(),
        );
        let cmd: CommonCommand = cmd.into();
        if block {
            self.inner.data_requester.submit_wait(cmd, None)
        } else {
            self.inner.data_requester.submit(cmd)
        }
    }

    pub fn send_write_page(
        &self,
        lba_start: u64,
        dptr: Dptr,
        nr_blocks_per_page: usize,
        block: bool,
    ) -> Option<InflightRequest<'_>> {
        let cmd = nvme::nvm::WriteCommand::new(
            CommandId::new(),
            NamespaceId::new(1u32),
            dptr,
            lba_start,
            nr_blocks_per_page as u16,
            WriteDword13::default(),
        );
        let cmd: CommonCommand = cmd.into();
        if block {
            self.inner.data_requester.submit_wait(cmd, None)
        } else {
            self.inner.data_requester.submit(cmd)
        }
    }

    pub async fn async_read_page(
        &self,
        lba_start: u64,
        out_buffer: &mut [u8],
        offset: usize,
    ) -> std::io::Result<()> {
        let start = Instant::now();
        let nr_blocks = DMA_PAGE_SIZE / self.blocking_get_lba_size();
        let buffer = self
            .inner
            .dma_pool
            .dma
            .allocate([0u8; DMA_PAGE_SIZE])
            .unwrap();
        let mut buffer = NvmeDmaRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.inner.dma_pool,
            )
            .unwrap();
        // TODO: queue full
        let inflight = self
            .send_read_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        let cc = inflight.await?;
        tracing::trace!("blocking read took {}us", start.elapsed().as_micros());

        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        buffer.dma_region().with(|data| {
            out_buffer.copy_from_slice(&data[offset..DMA_PAGE_SIZE]);
            Ok(())
        })
    }

    pub fn blocking_read_page(
        &self,
        lba_start: u64,
        out_buffer: &mut [u8],
        offset: usize,
    ) -> std::io::Result<()> {
        let start = Instant::now();
        let nr_blocks = DMA_PAGE_SIZE / self.blocking_get_lba_size();
        let buffer = self
            .inner
            .dma_pool
            .dma
            .allocate([0u8; DMA_PAGE_SIZE])
            .unwrap();
        let mut buffer = NvmeDmaRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.inner.dma_pool,
            )
            .unwrap();
        // TODO: queue full
        let inflight = self
            .send_read_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        let cc = inflight.wait()?;
        tracing::trace!("blocking read took {}us", start.elapsed().as_micros());

        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        buffer.dma_region().with(|data| {
            out_buffer.copy_from_slice(&data[offset..DMA_PAGE_SIZE]);
            Ok(())
        })
    }

    pub async fn async_write_page(
        &self,
        lba_start: u64,
        in_buffer: &[u8],
        offset: usize,
    ) -> std::io::Result<()> {
        let nr_blocks = DMA_PAGE_SIZE / self.blocking_get_lba_size();
        let mut buffer = self
            .inner
            .dma_pool
            .dma
            .allocate([0u8; DMA_PAGE_SIZE])
            .unwrap();
        buffer.with_mut(|data| data[offset..(offset + in_buffer.len())].copy_from_slice(in_buffer));
        let mut buffer = NvmeDmaRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.inner.dma_pool,
            )
            .unwrap();
        // TODO: queue full
        let inflight = self
            .send_write_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        let cc = inflight.await?;

        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        Ok(())
    }

    pub fn blocking_write_page(
        &self,
        lba_start: u64,
        in_buffer: &[u8],
        offset: usize,
    ) -> std::io::Result<()> {
        let nr_blocks = DMA_PAGE_SIZE / self.blocking_get_lba_size();
        let mut buffer = self
            .inner
            .dma_pool
            .dma
            .allocate([0u8; DMA_PAGE_SIZE])
            .unwrap();
        buffer.with_mut(|data| data[offset..(offset + in_buffer.len())].copy_from_slice(in_buffer));
        let mut buffer = NvmeDmaRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.inner.dma_pool,
            )
            .unwrap();
        // TODO: queue full
        let inflight = self
            .send_write_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        let cc = inflight.wait()?;

        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        Ok(())
    }

    pub fn sequential_write<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
    ) -> std::io::Result<usize> {
        // TODO: get from controller
        let count = phys.len().min(128);
        let dptr = super::dma::get_prp_list_or_buffer(
            &phys[0..count],
            &self.inner.dma_pool,
            PrpMode::Double,
        )
        .prp_list_or_buffer()
        .dptr();
        let lba_size = self.blocking_get_lba_size();
        let lbas_per_page = PAGE_SIZE / lba_size;
        let lba_start = disk_page_start * lbas_per_page as u64;
        let nr_blocks = count * lbas_per_page;
        let inflight = self
            .send_write_page(lba_start, dptr, nr_blocks, true)
            .unwrap();
        /*
        let cc = loop {
            inflight.req.get_completion();
            if let Ok(cc) = inflight.poll() {
                if cc.command_id() == inflight.id.into() {
                    break cc;
                }
            }
        };
        */
        let cc = inflight.wait()?;

        if cc.status().is_error() {
            tracing::warn!("got nvme error: {:?}", cc);
            return Err(ErrorKind::Other.into());
        }
        Ok(count)
    }

    pub fn sequential_read<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
    ) -> std::io::Result<usize> {
        // TODO: get from controller
        let start = Instant::now();
        let count = phys.len().min(128);
        let dptr = super::dma::get_prp_list_or_buffer(
            &phys[0..count],
            &self.inner.dma_pool,
            PrpMode::Double,
        )
        .prp_list_or_buffer()
        .dptr();
        let lba_size = self.blocking_get_lba_size();
        let lbas_per_page = PAGE_SIZE / lba_size;
        let lba_start = disk_page_start * lbas_per_page as u64;
        let nr_blocks = count * lbas_per_page;
        let inflight = self
            .send_read_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        let cc = inflight.wait()?;
        tracing::trace!("seq read took {}us", start.elapsed().as_micros());

        if cc.status().is_error() {
            tracing::warn!("got nvme error: {:?}", cc);
            return Err(ErrorKind::Other.into());
        }
        Ok(count)
    }

    pub async fn sequential_read_async<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
    ) -> std::io::Result<usize> {
        // TODO: get from controller
        let start = Instant::now();
        let count = phys.len().min(128);
        let dptr = super::dma::get_prp_list_or_buffer(
            &phys[0..count],
            &self.inner.dma_pool,
            PrpMode::Double,
        )
        .prp_list_or_buffer()
        .dptr();
        let lba_size = self.blocking_get_lba_size();
        let lbas_per_page = PAGE_SIZE / lba_size;
        let lba_start = disk_page_start * lbas_per_page as u64;
        let nr_blocks = count * lbas_per_page;
        let inflight = self
            .send_read_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        let cc = inflight.await?;
        tracing::trace!("async seq read took {}us", start.elapsed().as_micros());

        if cc.status().is_error() {
            tracing::warn!("got nvme error: {:?}", cc);
            return Err(ErrorKind::Other.into());
        }
        Ok(count)
    }

    pub fn blocking_write_pages<const NR: usize>(
        &self,
        lba_start: u64,
        in_buffer: &[u8],
    ) -> std::io::Result<()> {
        let nr_blocks = DMA_PAGE_SIZE * NR / self.blocking_get_lba_size();
        let mut buffer = self
            .inner
            .dma_pool
            .dma
            .allocate_array(NR * DMA_PAGE_SIZE, 0u8)
            .unwrap();
        buffer.with_mut(0..buffer.len(), |data| data.copy_from_slice(in_buffer));
        let mut buffer = NvmeDmaSliceRegion::new(buffer);
        let dptr = (&mut buffer)
            .get_dptr(
                nvme::hosted::memory::DptrMode::Prp(PrpMode::Double),
                &self.inner.dma_pool,
            )
            .unwrap();
        // TODO: queue full
        let inflight = self
            .send_write_page(lba_start, dptr, nr_blocks, true)
            .unwrap();

        /*
        let cc = loop {
            inflight.req.get_completion();
            if let Ok(cc) = inflight.poll() {
                if cc.command_id() == inflight.id.into() {
                    break cc;
                }
            }
        };
        */
        let cc = inflight.wait()?;

        if cc.status().is_error() {
            return Err(ErrorKind::Other.into());
        }
        Ok(())
    }
}

impl<'a> Future for InflightRequest<'a> {
    type Output = std::io::Result<CommonCompletion>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.req.async_poll(&*self, cx)
    }
}
