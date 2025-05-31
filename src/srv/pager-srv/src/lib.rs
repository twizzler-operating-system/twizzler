#![feature(naked_functions)]
#![feature(io_error_more)]

use std::sync::{Arc, OnceLock};

use async_executor::Executor;
use async_io::block_on;
use disk::{Disk, DiskPageRequest};
use object_store::{ExternalFile, LetheIoWrapper, PagedObjectStore};
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
mod nvme;
mod physrw;
mod request_handle;

pub use handle::{pager_close_handle, pager_open_handle};

pub static EXECUTOR: OnceLock<Executor> = OnceLock::new();

/***
 * Tracing Init
 */
fn tracing_init() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
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
    tracing::debug!("init start");
    let data = data_structure_init();
    let (rq, sq) = queue_init(q1, q2);
    let ex = async_runtime_init(4);

    tracing::debug!("init complete");
    return (rq, sq, data, ex);
}

fn spawn_queues(
    ctx: &'static PagerContext,
    kernel_rq: twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    ex: &'static Executor<'static>,
) {
    tracing::debug!("spawning queues...");
    ex.spawn(listen_queue(kernel_rq, ctx, handle_kernel_request, ex))
        .detach();
}

async fn listen_queue<R, C, F>(
    kernel_rq: twizzler_queue::CallbackQueueReceiver<R, C>,
    ctx: &'static PagerContext,
    handler: impl Fn(&'static PagerContext, R) -> F + Copy + Send + Sync + 'static,
    ex: &'static Executor<'static>,
) where
    F: std::future::Future<Output = C> + Send + 'static,
    R: std::fmt::Debug + Copy + Send + Sync + 'static,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    let q = Arc::new(kernel_rq);
    loop {
        tracing::trace!("queue receiving...");
        let (id, request) = q.receive().await.unwrap();
        tracing::trace!("got request: ({},{:?})", id, request);

        let qc = Arc::clone(&q);
        ex.spawn(async move {
            let comp = handler(ctx, request).await;
            notify(&qc, id, comp).await;
        })
        .detach();
    }
}

async fn notify<R, C>(q: &Arc<twizzler_queue::CallbackQueueReceiver<R, C>>, id: u32, res: C)
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    q.complete(id, res).await.unwrap();
    tracing::trace!("request {} complete", id);
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
    paged_ostore: Box<dyn PagedObjectStore<DiskPageRequest> + 'static + Sync + Send>,
    disk: Disk,
}

impl PagerContext {
    pub fn enumerate_external(&self, id: ObjID) -> Result<Vec<ExternalFile>, TwzError> {
        Ok(self
            .paged_ostore
            .enumerate_external(id.raw())?
            .iter()
            .cloned()
            .collect())
    }
}

static PAGER_CTX: OnceLock<PagerContext> = OnceLock::new();

fn do_pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    let (rq, sq, data, ex) = pager_init(q1, q2);
    let disk = block_on(ex.run(Disk::new(ex))).unwrap();
    let diskc = disk.clone();
    let disk = LetheIoWrapper::new(disk);
    let ostore = object_store::LetheObjectStore::open(disk.clone(), [0; 32]);
    let (name, ostore): (
        &str,
        Box<dyn PagedObjectStore<DiskPageRequest> + Send + Sync + 'static>,
    ) = match ostore {
        Ok(o) => ("lethe", Box::new(o)),
        Err(_) => (
            "ext2",
            Box::new(object_store::Ext2ObjectStore::new(disk.into_inner(), 0).unwrap()),
        ),
    };
    tracing::info!("opened object store as {}", name);
    let sq = Arc::new(sq);
    let _ = PAGER_CTX.set(PagerContext {
        data,
        sender: sq,
        paged_ostore: ostore,
        disk: diskc,
    });
    let ctx = PAGER_CTX.get().unwrap();
    spawn_queues(ctx, rq, ex);

    block_on(ex.run(async move {
        let _ = report_ready(&ctx, ex).await.unwrap();
    }));
    tracing::info!("pager ready");

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
pub fn full_object_sync(id: ObjID) -> Result<(), TwzError> {
    let task = EXECUTOR.get().unwrap().spawn(async move {
        let pager = PAGER_CTX.get().unwrap();
        tracing::debug!("starting full object sync for {:?}", id);
        pager.data.sync(&pager, id).await;
    });
    block_on(EXECUTOR.get().unwrap().run(async { task.await }));
    Ok(())
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
