#![feature(naked_functions)]
#![feature(io_error_more)]
#![feature(test)]

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use async_executor::Executor;
use async_io::{block_on, Timer};
use disk::{Disk, DiskPageRequest};
use object_store::{Ext4Store, ExternalFile, PagedObjectStore};
use tracing_subscriber::fmt::format::FmtSpan;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjID, Object, ObjectBuilder},
};
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, PagerCompletionData, RequestFromKernel, RequestFromPager,
};
use twizzler_queue::{QueueBase, QueueSender};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

use crate::{data::PagerData, request_handle::handle_kernel_request};

mod data;
mod disk;
mod handle;
mod helpers;
mod memstore;
mod nvme;
mod physrw;
mod request_handle;
mod stats;

pub use handle::{pager_close_handle, pager_open_handle};

pub static EXECUTOR: OnceLock<Executor> = OnceLock::new();

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
) -> Result<Q, String> {
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
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
) {
    let rq = attach_queue::<RequestFromKernel, CompletionToKernel, _>(
        q1,
        twizzler_queue::CallbackQueueReceiver::new,
    )
    .unwrap();
    let sq = attach_queue::<RequestFromPager, CompletionToPager, _>(
        q2,
        twizzler_queue::QueueSender::new,
    )
    .unwrap();

    return (rq, sq);
}

/***
 * Async Runtime Initialization
 * Creating n threads
 */
fn async_runtime_init(n: i32) -> &'static Executor<'static> {
    let ex = EXECUTOR.get_or_init(|| Executor::new());

    for _ in 0..n {
        std::thread::spawn(|| block_on(ex.run(std::future::pending::<()>())));
    }

    return ex;
}

/***
 * Pager Initialization generic function which calls specific initialization functions
 */
fn pager_init(
    q1: ObjID,
    q2: ObjID,
) -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    PagerData,
    &'static Executor<'static>,
) {
    tracing_init();
    let data = data_structure_init();
    let ex = async_runtime_init(4);
    let (rq, sq) = queue_init(q1, q2);

    tracing::debug!("init complete");
    return (rq, sq, data, ex);
}

fn spawn_queues(
    ctx: &'static PagerContext,
    kernel_rq: Arc<twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>>,
    ex: &'static Executor<'static>,
) {
    tracing::debug!("spawning queues...");
    ex.spawn(listen_queue(kernel_rq, ctx, handle_kernel_request, ex))
        .detach();
}

async fn listen_queue<R, C, F, I>(
    kernel_rq: Arc<twizzler_queue::CallbackQueueReceiver<R, C>>,
    ctx: &'static PagerContext,
    handler: impl Fn(&'static PagerContext, u32, R) -> F + Copy + Send + Sync + 'static,
    _ex: &'static Executor<'static>,
) where
    F: std::future::Future<Output = I> + Send + 'static,
    R: std::fmt::Debug + Copy + Send + Sync + 'static,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
    I: IntoIterator<Item = C> + Send + Sync + 'static,
{
    loop {
        tracing::trace!("queue receiving...");
        let (id, request) = kernel_rq.receive().await.unwrap();
        tracing::trace!("got request: ({},{:?})", id, request);

        let comp = handler(ctx, id, request).await;
        for comp in comp {
            notify(&kernel_rq, id, comp).await;
        }
    }
}

async fn notify<R, C>(q: &Arc<twizzler_queue::CallbackQueueReceiver<R, C>>, id: u32, res: C)
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    q.complete(id, res).await.unwrap();
    //tracing::trace!("request {} complete", id);
}

async fn report_ready(
    ctx: &PagerContext,
    _ex: &'static Executor<'static>,
) -> Option<PagerCompletionData> {
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
    kernel_notify:
        Arc<twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>>,
    paged_ostore: Box<dyn PagedObjectStore<DiskPageRequest> + 'static + Sync + Send>,
    disk: Disk,
}

impl PagerContext {
    pub async fn enumerate_external(
        &'static self,
        id: ObjID,
    ) -> Result<Vec<ExternalFile>, TwzError> {
        blocking::unblock(move || {
            Ok(self
                .paged_ostore
                .enumerate_external(id.raw())?
                .iter()
                .cloned()
                .collect())
        })
        .await
    }

    pub async fn notify_kernel(&'static self, id: u32, comp: CompletionToKernel) {
        notify(&self.kernel_notify, id, comp).await;
    }
}

static PAGER_CTX: OnceLock<PagerContext> = OnceLock::new();

fn do_pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    let (rq, sq, data, ex) = pager_init(q1, q2);
    let disk = block_on(ex.run(Disk::new(ex))).unwrap();
    let diskc = disk.clone();

    let ext4_store = Ext4Store::<DiskPageRequest>::new(disk, "/").unwrap();

    let sq = Arc::new(sq);
    let rq = Arc::new(rq);
    let _ = PAGER_CTX.set(PagerContext {
        data,
        sender: sq,
        kernel_notify: rq.clone(),
        paged_ostore: Box::new(ext4_store),
        disk: diskc,
    });
    let ctx = PAGER_CTX.get().unwrap();

    spawn_queues(ctx, rq, ex);

    block_on(ex.run(async move {
        let _ = report_ready(&ctx, ex).await.unwrap();
    }));
    tracing::info!("pager ready");

    //disk::benches::bench_disk(ctx);
    if true {
        let _ = ex
            .spawn(async {
                let pager = PAGER_CTX.get().unwrap();
                loop {
                    pager.data.print_stats();
                    pager.data.reset_stats();
                    Timer::after(Duration::from_millis(1000)).await;
                }
            })
            .detach();
    }

    let bootstrap_id = ctx.paged_ostore.get_config_id().unwrap_or_else(|_| {
        tracing::info!("creating new naming object");
        let vo = VecObject::<u32, VecObjectAlloc>::new(ObjectBuilder::default().persist()).unwrap();
        ctx.paged_ostore
            .set_config_id(vo.object().id().raw())
            .unwrap();
        vo.object().id().raw()
    });
    tracing::info!("found root namespace: {:x}", bootstrap_id);

    return bootstrap_id.into();
}

#[secgate::secure_gate]
pub fn pager_start(q1: ObjID, q2: ObjID) -> Result<ObjID, TwzError> {
    Ok(do_pager_start(q1, q2))
}

#[secgate::secure_gate]
pub fn adv_lethe() -> Result<(), TwzError> {
    PAGER_CTX.get().unwrap().paged_ostore.flush().unwrap();
    Ok(())
}

#[secgate::secure_gate]
pub fn disk_len(id: ObjID) -> Result<u64, TwzError> {
    PAGER_CTX
        .get()
        .unwrap()
        .paged_ostore
        .len(id.raw())
        // TODO: err
        .map_err(|_| TwzError::NOT_SUPPORTED)
}
