#![feature(naked_functions)]
#![feature(io_error_more)]
#![feature(test)]
#![feature(thread_local)]

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use async_io::Timer;
use disk::Disk;
use memstore::virtio::init_virtio;
use object_store::{Ext4Store, ExternalFile, PagedObjectStore};
use physrw::init_pr_mgr;
use threads::{run_async, spawn_async, PagerThreadPool};
use tracing_subscriber::fmt::format::FmtSpan;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjID, Object, ObjectBuilder},
    Result,
};
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, PagerCompletionData, RequestFromKernel, RequestFromPager,
};
use twizzler_queue::{QueueBase, QueueSender, SubmissionFlags};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

use crate::data::PagerData;

mod data;
mod disk;
mod handle;
mod helpers;
// in-progress
#[allow(unused)]
mod memstore;
mod nvme;
mod physrw;
mod request_handle;
mod stats;
mod threads;

pub use handle::{pager_close_handle, pager_open_handle};

/***
 * Tracing Init
 */
fn tracing_init() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_span_events(FmtSpan::ENTER)
            .without_time()
            .finish(),
    )
    .unwrap();
    tracing_log::LogTracer::init().unwrap();
}

/***
 * Pager Data Structures Initialization
 */
fn data_structure_init() -> PagerData {
    let pager_data = PagerData::new();

    return pager_data;
}

/***
 * Queue Initializing
 */
fn attach_queue<T: std::marker::Copy, U: std::marker::Copy, Q>(
    obj_id: ObjID,
    queue_constructor: impl FnOnce(twizzler_queue::Queue<T, U>) -> Q,
) -> Result<Q> {
    tracing::debug!("Pager Attaching Queue: {}", obj_id);

    let object = unsafe {
        Object::<QueueBase<T, U>>::map_unchecked(obj_id, MapFlags::READ | MapFlags::WRITE).unwrap()
    };

    tracing::debug!("queue mapped; constructing...");
    // Ensure the object is cast or transformed to match the expected `Queue` type
    let queue: twizzler_queue::Queue<T, U> = twizzler_queue::Queue::from(object.into_handle());
    Ok(queue_constructor(queue))
}

fn queue_init(
    q1: ObjID,
    q2: ObjID,
) -> (
    twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
) {
    let rq = attach_queue::<RequestFromKernel, CompletionToKernel, _>(q1, |q| q).unwrap();
    let sq = attach_queue::<RequestFromPager, CompletionToPager, _>(
        q2,
        twizzler_queue::QueueSender::new,
    )
    .unwrap();

    return (rq, sq);
}

/***
 * Pager Initialization generic function which calls specific initialization functions
 */
fn pager_init(
    q1: ObjID,
    q2: ObjID,
) -> (
    &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    PagerData,
) {
    tracing_init();
    let data = data_structure_init();
    let (rq, sq) = queue_init(q1, q2);

    let rq = unsafe { Box::into_raw(Box::new(rq)).as_ref().unwrap() };
    tracing::debug!("init complete");
    return (rq, sq, data);
}

async fn report_ready(ctx: &PagerContext) -> Option<PagerCompletionData> {
    tracing::debug!("sending ready signal to kernel");
    let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::Ready);

    match ctx.sender.submit_and_wait(request).await {
        Ok(completion) => {
            tracing::debug!("received completion for ready signal: {:?}", completion);
            return Some(completion.data());
        }
        Err(e) => {
            tracing::warn!("error from ready signal {:?}", e);
            return None;
        }
    }
}

struct PagerContext {
    data: PagerData,
    sender: Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    kernel_notify: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
    pool: PagerThreadPool,

    store: OnceLock<Ext4Store<Disk>>,
}

impl PagerContext {
    pub fn paged_ostore(&self, _id: Option<ObjID>) -> Result<&Ext4Store<Disk>> {
        Ok(self.store.wait())
    }

    pub async fn enumerate_external(&'static self, id: ObjID) -> Result<Vec<ExternalFile>> {
        Ok(self
            .paged_ostore(None)?
            .enumerate_external(id.raw())
            .await?
            .iter()
            .cloned()
            .collect())
    }

    pub fn notify_kernel(&'static self, id: u32, comp: CompletionToKernel) {
        self.kernel_notify
            .complete(id, comp, SubmissionFlags::empty())
            .unwrap();
    }
}

static PAGER_CTX: OnceLock<PagerContext> = OnceLock::new();

fn do_pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    let (rq, sq, data) = pager_init(q1, q2);
    let sq = Arc::new(sq);
    init_pr_mgr(sq.clone());
    #[allow(unused_variables)]
    let disk = run_async(Disk::new()).unwrap();

    let _ = PAGER_CTX.set(PagerContext {
        data,
        sender: sq,
        kernel_notify: rq,
        store: OnceLock::new(),
        pool: PagerThreadPool::new(rq),
    });
    let ctx = PAGER_CTX.get().unwrap();

    #[allow(unused_variables)]
    let virtio_store = run_async(init_virtio()).unwrap();
    let ext4_store = run_async(Ext4Store::new(disk.clone(), "/")).unwrap();

    let _ = ctx.store.set(ext4_store);

    run_async(async move {
        let _ = report_ready(&ctx).await.unwrap();
    });

    tracing::info!("pager ready");

    //disk::benches::bench_disk(ctx);
    if false {
        spawn_async(async {
            let pager = PAGER_CTX.get().unwrap();
            loop {
                pager.data.print_stats();
                pager.data.reset_stats();
                Timer::after(Duration::from_millis(1000)).await;
            }
        });
    }

    let bootstrap_id = ctx.paged_ostore(None).map_or(0u128, |po| {
        if let Ok(id) = run_async(po.get_config_id()) {
            id
        } else {
            tracing::info!("creating new naming object");
            let vo =
                VecObject::<u32, VecObjectAlloc>::new(ObjectBuilder::default().persist()).unwrap();
            run_async(po.set_config_id(vo.object().id().raw())).unwrap();
            vo.object().id().raw()
        }
    });
    tracing::info!("found root namespace: {:x}", bootstrap_id);

    return bootstrap_id.into();
}

#[secgate::secure_gate]
pub fn pager_start(q1: ObjID, q2: ObjID) -> Result<ObjID> {
    Ok(do_pager_start(q1, q2))
}

#[secgate::secure_gate]
pub fn adv_lethe() -> Result<()> {
    run_async(PAGER_CTX.get().unwrap().paged_ostore(None)?.flush()).unwrap();
    Ok(())
}

#[secgate::secure_gate]
pub fn disk_len(id: ObjID) -> Result<u64> {
    run_async(PAGER_CTX.get().unwrap().paged_ostore(None)?.len(id.raw()))
        // TODO: err
        .map_err(|_| TwzError::NOT_SUPPORTED)
}
