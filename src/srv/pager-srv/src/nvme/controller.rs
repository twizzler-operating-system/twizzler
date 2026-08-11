use std::{
    future::Future,
    io::ErrorKind,
    mem::size_of,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    thread::JoinHandle,
    time::Instant,
};

use nvme::{
    admin::{CreateIOCompletionQueue, CreateIOSubmissionQueue},
    ds::{
        cmd::admin::{features::FeatureId, AdminCommand},
        controller::properties::config::ControllerConfig,
        identify::{
            controller::IdentifyControllerDataStructure, namespace::IdentifyNamespaceDataStructure,
        },
        namespace::NamespaceId,
        queue::{
            comentry::CommonCompletion,
            subentry::{CommandDword0, CommonCommand, Dptr, FuseSpec, Psdt},
            CommandId, QueueId, QueuePriority,
        },
    },
    hosted::memory::{PhysicalPageCollection, PrpMode},
    nvm::{ReadDword13, WriteDword13},
};
use twizzler_abi::syscall::ThreadSyncSleep;
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
    /// One queue pair per pager worker, indexed by `threads::current_queue_index`, so submission
    /// never crosses threads and the requester lock is uncontended in the common case.
    data_requesters: Vec<NvmeRequester>,
    admin_requester: NvmeRequester,
    device: Device,
    dma_pool: Arc<CachedDmaPool>,
    /// Diagnostic: bumped once per interrupt-thread iteration and once per park. Two dumps taken
    /// apart say whether the only thread that reaps completions is still running at all.
    int_loops: AtomicU64,
    int_parks: AtomicU64,
}

pub struct NvmeController {
    inner: Arc<NvmeControllerInner>,
    capacity: OnceLock<usize>,
    block_size: OnceLock<usize>,
    max_transfer_pages: OnceLock<usize>,
    /// CAP.MPSMIN in bytes; MDTS is expressed in units of it.
    mps_min: usize,
    int_thrs: OnceLock<Vec<JoinHandle<()>>>,
}

const ADMIN_QUEUE_LEN: u16 = 32;
/// Queue ids are 1-based; 0 is the admin pair. Interrupt vectors follow the same numbering, so data
/// queue `i` uses qid `i + 1` and MSI-X entry `i + 1`, leaving vector 0 to the admin queue -- which
/// the spec fixes there and gives us no say in.
const DATA_QUEUE_ID: u16 = 1;
/// Ceiling on I/O queue pairs, whatever the controller and worker count would allow. `DeviceRepr`
/// has `NUM_DEVICE_INTERRUPTS` (32) interrupt slots and vector 0 is spoken for.
pub(crate) const MAX_DATA_QUEUES: usize = 16;
/// Create I/O Queue is issued with PC=1, so the queue memory must be physically contiguous, and a
/// DMA pool allocation is only guaranteed contiguous within a single page. At 64 bytes an entry
/// that caps the submission queue -- and hence the number of outstanding commands -- at one page's
/// worth of entries. Going deeper needs either a contiguity check on a multi-page pin or PC=0.
const MAX_DATA_QUEUE_LEN: u32 = (DMA_PAGE_SIZE / size_of::<CommonCommand>()) as u32;
/// Commands one `pipelined_transfer` keeps in flight. Four covers a 512-page large-page fill in a
/// single round on QEMU, where MDTS gives 128 pages per command -- that fill is the shape that
/// moves the most bytes. Deeper starts crowding a submission queue every worker shares, and the
/// first command of a batch is the only one allowed to wait for a slot, so a too-large value costs
/// other tasks their depth rather than buying this one any.
const PIPELINE_DEPTH: usize = 4;

/// Largest transfer we will build for one command when the controller reports no MDTS limit, and
/// the ceiling we clamp a reported limit to.
const MAX_TRANSFER_PAGES: usize = 512;

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

    // Read before `bar` is handed to the requester, which ends the borrow `reg` holds on it.
    let cap = map_field!(reg.capabilities).read();
    // MQES is 0-based.
    let data_queue_len = (cap.max_queue_entries() as u32 + 1).min(MAX_DATA_QUEUE_LEN) as usize;
    let mps_min = cap.memory_page_sz_min_bytes();

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

    let nr_queues = negotiate_queue_count(&admin_requester, crate::threads::desired_queues())?;
    // The pool sizes itself from this, one worker per queue pair, so it has to be recorded before
    // `PagerThreadPool::new` runs. A device may grant fewer than asked for, never more.
    crate::threads::set_granted_queues(nr_queues);
    let mut data_requesters = Vec::with_capacity(nr_queues);
    for i in 0..nr_queues {
        let id = ((DATA_QUEUE_ID as usize + i) as u16).into();
        let ivec = (DATA_QUEUE_ID as usize + i) as u16;
        device
            .allocate_interrupt(ivec as usize)
            .expect("failed to allocate nvme interrupt");
        data_requesters.push(NvmeController::create_queue_pair(
            &mut admin_requester,
            &dma_pool,
            &mut device,
            id,
            id,
            QueuePriority::Medium,
            data_queue_len,
            ivec,
        )?);
    }
    // Startup-only, and the first thing you want to know when a throughput number moves.
    tracing::info!(
        "nvme: {} io queue pairs of {} entries",
        nr_queues,
        data_queue_len
    );

    Ok(NvmeController {
        inner: Arc::new(NvmeControllerInner {
            data_requesters,
            admin_requester,
            device,
            dma_pool,
            int_loops: AtomicU64::new(0),
            int_parks: AtomicU64::new(0),
        }),
        capacity: OnceLock::new(),
        block_size: OnceLock::new(),
        max_transfer_pages: OnceLock::new(),
        mps_min,
        int_thrs: OnceLock::new(),
    })
}

/// Ask the controller for `want` I/O queue pairs and report what it actually allocated.
///
/// Built by hand rather than with `nvme::admin::SetFeatures`, which sends the wrong opcode
/// (`CreateCompletionQueue`) and offers no way to set CDW11, where the requested counts live.
fn negotiate_queue_count(admin: &NvmeRequester, want: usize) -> std::io::Result<usize> {
    let want = want.clamp(1, MAX_DATA_QUEUES);
    // NSQR and NCQR are 0-based counts in the low and high halves of CDW11; the completion reports
    // what was allocated the same way, and may be fewer than asked for but never more.
    let requested = (want - 1) as u32;
    let cmd = CommonCommand::new()
        .with_cdw0(CommandDword0::build(
            AdminCommand::SetFeatures.into(),
            CommandId::new(),
            FuseSpec::Normal,
            Psdt::Prp,
        ))
        .with_cdw10(FeatureId::NumberOfQueues as u32)
        .with_cdw11(requested | (requested << 16));

    let inflight = admin.submit(cmd).ok_or(ErrorKind::Other)?;
    let cc = loop {
        if let Some((id, resp)) = admin.get_completion() {
            if id != inflight.id {
                tracing::error!("got other command ID for set-features command");
            }
            break resp;
        }
    };
    if cc.status().is_error() {
        tracing::warn!("nvme set-features(num queues) failed: {:?}", cc);
        return Ok(1);
    }

    let dw0 = cc.dw0();
    let nsqa = (dw0 & 0xffff) as usize + 1;
    let ncqa = ((dw0 >> 16) & 0xffff) as usize + 1;
    Ok(nsqa.min(ncqa).min(want).max(1))
}

/// The one controller, for the park hooks below. They are called from `threads::park_poll`, which
/// has no path to a `PagerContext`, and the controller is a singleton in practice.
static PARK_CTRL: OnceLock<Arc<NvmeControllerInner>> = OnceLock::new();

fn park_queue(inner: &NvmeControllerInner) -> usize {
    crate::threads::current_queue_index() % inner.data_requesters.len()
}

/// Consume this thread's queue interrupt and drain its completions.
///
/// Consuming the interrupt word is not optional: `setup_interrupt_sleep` blocks only while it is
/// zero, so leaving it set turns the park below into a spin.
pub fn reap_current_queue() {
    let Some(inner) = PARK_CTRL.get() else {
        return;
    };
    let idx = park_queue(inner);
    inner
        .device
        .repr()
        .check_for_interrupt(DATA_QUEUE_ID as usize + idx);
    inner.data_requesters[idx].check_completions();
}

/// Sleep op for this thread's queue interrupt, to be armed alongside the park word.
pub fn current_queue_sleep() -> Option<ThreadSyncSleep> {
    let inner = PARK_CTRL.get()?;
    let idx = park_queue(inner);
    Some(
        inner
            .device
            .repr()
            .setup_interrupt_sleep(DATA_QUEUE_ID as usize + idx),
    )
}

fn interrupt_thread_main(inner: &NvmeControllerInner, inum: usize, queue: Option<usize>) {
    loop {
        inner.int_loops.fetch_add(1, Ordering::Relaxed);
        let more = inner.device.repr().check_for_interrupt(inum).is_some();

        // Only the vector-0 thread reaps admin: the admin completion queue is fixed to that vector,
        // and every requester is reaped by exactly one thread so they do not fight for its lock.
        let more_a = queue.is_none() && inner.admin_requester.check_completions();
        let more_d = queue.is_some_and(|q| inner.data_requesters[q].check_completions());

        if !more && !more_a && !more_d {
            inner.int_parks.fetch_add(1, Ordering::Relaxed);
            inner.device.repr().wait_for_interrupt(inum, None);
        }
    }
}

#[allow(dead_code)]
impl NvmeController {
    /// Called by the pager watchdog when a work item has been stuck long enough to report.
    pub fn dump_stall(&self) {
        tracing::warn!(
            "nvme dump: int_loops {} int_parks {}",
            self.inner.int_loops.load(Ordering::Relaxed),
            self.inner.int_parks.load(Ordering::Relaxed),
        );
        self.inner.admin_requester.dump("admin");
        for (i, req) in self.inner.data_requesters.iter().enumerate() {
            req.dump(&format!("data{}", i));
        }
    }

    pub fn new(device: Device) -> std::io::Result<Self> {
        let dma_pool = DmaPool::new(
            DmaPool::default_spec(),
            twizzler_driver::dma::Access::BiDirectional,
            DmaOptions::empty(),
        );

        let ctrl = init_controller(device, dma_pool)?;
        let _ = PARK_CTRL.set(ctrl.inner.clone());
        // Only the admin queue gets a reaper thread. Data queues are reaped by whichever thread is
        // waiting on them -- see `threads::park_poll` -- which is the whole point: a dedicated
        // reaper can only ever hand the completion to the thread that was already waiting for it.
        let inner = ctrl.inner.clone();
        ctrl.int_thrs
            .set(vec![std::thread::Builder::new()
                .name("nvme-int-admin".to_string())
                .spawn(move || {
                    interrupt_thread_main(&inner, 0, None);
                })
                .unwrap()])
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
        ivec: u16,
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
                ivec,
                true,
            );

            let cmd: CommonCommand = cmd.into();
            let inflight = admin_requester.submit(cmd).unwrap();
            loop {
                if let Some((_, resp)) = admin_requester.get_completion() {
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
                if let Some((_, resp)) = admin_requester.get_completion() {
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

    pub fn blocking_get_flash_size(&self) -> usize {
        if let Some(sz) = self.capacity.get() {
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
            let _ = self
                .capacity
                .set(ns.capacity as usize * self.blocking_get_lba_size());
            ns.capacity as usize * self.blocking_get_lba_size()
        }
    }

    /// Largest number of `PAGE_SIZE` pages the controller will accept in one command, from MDTS.
    pub fn blocking_get_max_transfer_pages<const PAGE_SIZE: usize>(&self) -> usize {
        *self.max_transfer_pages.get_or_init(|| {
            let (inflight, dma) = self.send_identify_controller().unwrap();
            let cc = inflight.wait().unwrap();
            if cc.status().is_error() {
                panic!("error on ident ctrl")
            }
            let mdts = dma.dma_region().with(|ident| ident.max_data_transfer_size);
            // MDTS is log2 of the transfer size in CAP.MPSMIN units; 0 means unlimited.
            let pages = if mdts == 0 || mdts as u32 >= usize::BITS {
                MAX_TRANSFER_PAGES
            } else {
                ((1usize << mdts) * self.mps_min) / PAGE_SIZE
            };
            let pages = pages.clamp(1, MAX_TRANSFER_PAGES);
            tracing::debug!("nvme max transfer: {} pages (mdts {})", pages, mdts);
            pages
        })
    }

    /// The queue pair owned by the calling thread. Submission and its completion always land on the
    /// same requester because `InflightRequest` carries the reference it was submitted through, and
    /// tasks never migrate off a worker's thread-local executor.
    fn data_requester(&self) -> &NvmeRequester {
        let reqs = &self.inner.data_requesters;
        &reqs[crate::threads::current_queue_index() % reqs.len()]
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
        let req = self.data_requester();
        if block {
            req.submit_wait(cmd, None)
        } else {
            req.submit(cmd)
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
        let req = self.data_requester();
        if block {
            req.submit_wait(cmd, None)
        } else {
            req.submit(cmd)
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
        let buffer = self.inner.dma_pool.get_page().unwrap();
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

        let cc = inflight
            .await
            .inspect_err(|e| tracing::warn!("nvme err async_r_p: {}", e))?;
        tracing::trace!("async read took {}us", start.elapsed().as_micros());

        if cc.status().is_error() {
            tracing::warn!("got nvme err arp: {:?}", cc);
            return Err(ErrorKind::Other.into());
        }
        buffer.dma_region().with(|data| {
            out_buffer.copy_from_slice(&data[offset..DMA_PAGE_SIZE]);
        });
        self.inner.dma_pool.put_page(buffer.into_inner());

        Ok(())
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
        let start = Instant::now();
        let nr_blocks = DMA_PAGE_SIZE / self.blocking_get_lba_size();
        let mut buffer = self.inner.dma_pool.get_page().unwrap();
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

        let cc = inflight
            .await
            .inspect_err(|e| tracing::warn!("nvme err async_w_p: {}", e))?;
        tracing::trace!("async write took {}us", start.elapsed().as_micros());
        self.inner.dma_pool.put_page(buffer.into_inner());

        if cc.status().is_error() {
            tracing::warn!("got nvme err awp: {:?}", cc);
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
        let count = phys
            .len()
            .min(self.blocking_get_max_transfer_pages::<PAGE_SIZE>());
        // Bound to a local, not consumed as a temporary. Dropping the `PrpMgr` hands its list
        // pages back to the pool, and the device reads that list when it *processes* the command,
        // which can be long after we ring the doorbell. As a temporary the pages were recycled
        // before submission, so a concurrent command's list overwrote the one still in use -- both
        // lists are valid addresses, so the transfer silently lands in the wrong pages rather than
        // failing. Held here until after the wait below.
        let prp = super::dma::get_prp_list_or_buffer(
            &phys[0..count],
            &self.inner.dma_pool,
            PrpMode::Double,
        );
        let dptr = prp.prp_list_or_buffer().dptr();
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
            tracing::warn!("got nvme error sw: {:?}", cc);
            return Err(ErrorKind::Other.into());
        }
        Ok(count)
    }

    pub fn sequential_read<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
    ) -> std::io::Result<usize> {
        let start = Instant::now();
        let count = phys
            .len()
            .min(self.blocking_get_max_transfer_pages::<PAGE_SIZE>());
        // Bound to a local, not consumed as a temporary. Dropping the `PrpMgr` hands its list
        // pages back to the pool, and the device reads that list when it *processes* the command,
        // which can be long after we ring the doorbell. As a temporary the pages were recycled
        // before submission, so a concurrent command's list overwrote the one still in use -- both
        // lists are valid addresses, so the transfer silently lands in the wrong pages rather than
        // failing. Held here until after the wait below.
        let prp = super::dma::get_prp_list_or_buffer(
            &phys[0..count],
            &self.inner.dma_pool,
            PrpMode::Double,
        );
        let dptr = prp.prp_list_or_buffer().dptr();
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
            tracing::warn!("got nvme error sr: {:?}", cc);
            return Err(ErrorKind::Other.into());
        }
        Ok(count)
    }

    /// Largest run one pipelined call will describe, so a caller building descriptors for a long
    /// extent stays linear in its length rather than rebuilding the tail per command.
    pub fn blocking_get_pipelined_pages<const PAGE_SIZE: usize>(&self) -> usize {
        self.blocking_get_max_transfer_pages::<PAGE_SIZE>() * PIPELINE_DEPTH
    }

    /// Transfer `phys` at the device's own depth: every command goes out before any is awaited.
    /// One extent run used to go one command at a time because the caller re-derived its position
    /// from each completion, which pinned a 2 MiB fill to four serial round trips no matter how
    /// deep the queue was.
    ///
    /// Two invariants shape the awkward parts:
    ///
    /// - Every `PrpMgr` lives until its own command completes. Dropping one returns its list pages
    ///   to the pool while the device may still be reading them, and the next command to allocate
    ///   overwrites a list in use -- both hold valid addresses, so the transfer lands in the wrong
    ///   pages silently instead of failing. That is why the loop below awaits the whole batch
    ///   rather than returning on the first error.
    /// - Only the first command may block for a submission slot. That guarantees forward progress
    ///   without ever parking the executor thread just to go deeper: if the queue is full the batch
    ///   is simply shorter, and the caller loops for the rest.
    fn pipelined_transfer<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
        write: bool,
    ) -> std::io::Result<usize> {
        let start = Instant::now();
        let max = self.blocking_get_max_transfer_pages::<PAGE_SIZE>();
        let lbas_per_page = PAGE_SIZE / self.blocking_get_lba_size();

        let mut batch = Vec::with_capacity(PIPELINE_DEPTH);
        let mut submitted = 0usize;
        for chunk in phys.chunks(max).take(PIPELINE_DEPTH) {
            let prp =
                super::dma::get_prp_list_or_buffer(chunk, &self.inner.dma_pool, PrpMode::Double);
            let dptr = prp.prp_list_or_buffer().dptr();
            let lba_start = (disk_page_start + submitted as u64) * lbas_per_page as u64;
            let nr_blocks = chunk.len() * lbas_per_page;
            let block = batch.is_empty();
            let inflight = if write {
                self.send_write_page(lba_start, dptr, nr_blocks, block)
            } else {
                self.send_read_page(lba_start, dptr, nr_blocks, block)
            };
            let Some(inflight) = inflight else {
                break;
            };
            batch.push((prp, inflight, chunk.len()));
            submitted += chunk.len();
        }

        let nr_cmds = batch.len();
        // Report the leading run that succeeded: the caller advances by a count, so a gap in the
        // middle cannot be expressed. It re-reads from there, and the kernel re-faults for the
        // rest.
        let mut done = 0;
        let mut good = true;
        let mut err = None;
        for (prp, inflight, pages) in batch {
            match inflight.wait_owned() {
                Ok(cc) if !cc.status().is_error() => {
                    if good {
                        done += pages;
                    }
                }
                Ok(cc) => {
                    tracing::warn!("got nvme error (write = {}): {:?}", write, cc);
                    good = false;
                    err.get_or_insert(std::io::Error::from(ErrorKind::Other));
                }
                Err(e) => {
                    tracing::warn!("nvme err (write = {}): {}", write, e);
                    good = false;
                    err.get_or_insert(e);
                }
            }
            drop(prp);
        }
        tracing::trace!(
            "seq {} took {}us ({} pages in {} commands)",
            if write { "write" } else { "read" },
            start.elapsed().as_micros(),
            done,
            nr_cmds,
        );

        match err {
            Some(e) if done == 0 => Err(e),
            _ => Ok(done),
        }
    }

    pub async fn sequential_read_async<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
    ) -> std::io::Result<usize> {
        self.pipelined_transfer::<PAGE_SIZE>(disk_page_start, phys, false)
    }

    pub async fn sequential_write_async<const PAGE_SIZE: usize>(
        &self,
        disk_page_start: u64,
        phys: &[PhysInfo],
    ) -> std::io::Result<usize> {
        self.pipelined_transfer::<PAGE_SIZE>(disk_page_start, phys, true)
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
        self.poll_completion(cx)
    }
}
