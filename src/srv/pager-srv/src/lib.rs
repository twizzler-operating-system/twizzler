#![feature(ptr_sub_ptr)]
#![feature(naked_functions)]

use std::sync::{Arc, OnceLock};

use async_executor::Executor;
use async_io::block_on;
use secgate::secure_gate;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjectBuilder, RawObject},
};
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, PagerCompletionData, PhysRange, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use twizzler_queue::QueueSender;

use crate::{data::PagerData, helpers::physrange_to_pages, request_handle::handle_kernel_request};

mod data;
mod helpers;
mod physrw;
mod request_handle;

pub static EXECUTOR: OnceLock<Executor> = OnceLock::new();

/***
 * Tracing Init
 */
fn tracing_init() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .finish(),
    )
    .unwrap();
}

/***
 * Pager Data Structures Initialization
 */
fn data_structure_init() -> PagerData {
    let pager_data = PagerData::new();

    return pager_data;
}

/***
 * Setup data structures and physical memory for use by pager
 */
fn memory_init(data: PagerData, range: PhysRange) {
    data.init_range(range);
    let pages = physrange_to_pages(&range) as usize;
    data.resize(pages);
}

/***
 * Queue Initializing
 */
fn attach_queue<T: std::marker::Copy, U: std::marker::Copy, Q>(
    obj_id: ObjID,
    queue_constructor: impl FnOnce(twizzler_queue::Queue<T, U>) -> Q,
) -> Result<Q, String> {
    tracing::debug!("Pager Attaching Queue: {}", obj_id);

    let object = Object::init_id(
        obj_id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();

    // Ensure the object is cast or transformed to match the expected `Queue` type
    let queue: twizzler_queue::Queue<T, U> = twizzler_queue::Queue::from(object);

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
    pager_rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    kernel_rq: twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    data: PagerData,
    ex: &'static Executor<'static>,
) {
    tracing::debug!("spawning queues...");
    ex.spawn(listen_queue(
        pager_rq.clone(),
        kernel_rq,
        data,
        handle_kernel_request,
        ex,
    ))
    .detach();
}

async fn listen_queue<R, C, PR, PC, F>(
    pager_rq: Arc<QueueSender<PR, PC>>,
    kernel_rq: twizzler_queue::CallbackQueueReceiver<R, C>,
    data: PagerData,
    handler: impl Fn(Arc<QueueSender<PR, PC>>, R, Arc<PagerData>) -> F + Copy + Send + Sync + 'static,
    ex: &'static Executor<'static>,
) where
    F: std::future::Future<Output = Option<C>> + Send + 'static,
    R: std::fmt::Debug + Copy + Send + Sync + 'static,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
    PR: std::fmt::Debug + Copy + Send + Sync + 'static,
    PC: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    let q = Arc::new(kernel_rq);
    let data = Arc::new(data);
    loop {
        tracing::trace!("queue receiving...");
        let (id, request) = q.receive().await.unwrap();
        tracing::trace!("got request: ({},{:?})", id, request);

        let qc = Arc::clone(&q);
        let datac = Arc::clone(&data);
        let prq = pager_rq.clone();
        ex.spawn(async move {
            let comp = handler(prq, request, datac).await;
            notify(&qc, id, comp).await;
        })
        .detach();
    }
}

async fn notify<R, C>(q: &Arc<twizzler_queue::CallbackQueueReceiver<R, C>>, id: u32, res: Option<C>)
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    if let Some(res) = res {
        q.complete(id, res).await.unwrap();
    }
    tracing::trace!("request {} complete", id);
}

async fn report_ready(
    q: &Arc<twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>>,
    _ex: &'static Executor<'static>,
) -> Option<PagerCompletionData> {
    tracing::debug!("sending ready signal to kernel");
    let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::Ready);

    match q.submit_and_wait(request).await {
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

static PAGER_DATA: OnceLock<(
    PagerData,
    Arc<QueueSender<RequestFromPager, CompletionToPager>>,
)> = OnceLock::new();

fn do_pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    let (rq, sq, data, ex) = pager_init(q1, q2);
    object_store::init(ex);
    let sq = Arc::new(sq);
    let sqc = sq.clone();
    spawn_queues(&sq, rq, data.clone(), ex);

    let phys_range: Option<PhysRange> = block_on(async move {
        let res = report_ready(&sqc, ex).await;
        match res {
            Some(PagerCompletionData::DramPages(range)) => {
                Some(range) // Return the range
            }
            _ => {
                tracing::error!("ERROR: no range from ready request");
                None
            }
        }
    });

    if let Some(range) = phys_range {
        tracing::info!(
            "initializing the pager with physical memory range: start: {}, end: {}",
            range.start,
            range.end
        );
        memory_init(data.clone(), range);
    } else {
        tracing::error!("cannot complete pager initialization with no physical memory");
    }
    tracing::info!("pager ready");

    let _ = PAGER_DATA.set((data, sq));

    let bootstrap_id = object_store::get_config_id().unwrap().unwrap_or_else(|| {
        tracing::info!("creating new naming object");
        let vo = VecObject::<u32, VecObjectAlloc>::new(ObjectBuilder::default().persist()).unwrap();
        object_store::set_config_id(vo.object().id().raw()).unwrap();
        vo.object().id().raw()
    });
    tracing::info!("found root namespace: {:x}", bootstrap_id);

    return bootstrap_id.into();

    object_store::create_object(17).unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("newobj {:#?}", khf);
    });
    object_store::write_all(17, b"this is a test", 0).unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("written {:#?}", khf);
    });

    object_store::advance_epoch().unwrap();
    object_store::with_khf(|khf| {
        tracing::info!("written-adv {:#?}", khf);
    });

    object_store::unlink_object(17).unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("removed {:#?}", khf);
    });
    object_store::advance_epoch().unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("removed-adv {:#?}", khf);
    });

    loop {}
    let mut buf = [0; 12];
    object_store::read_exact(0x5d74fb7c3fe55e64131351157f1fd996u128, &mut buf, 0).unwrap();
    println!("==> {}", String::from_utf8_lossy(&buf));
    object_store::advance_epoch().unwrap();
    object_store::read_exact(17, &mut buf, 0).unwrap();
    println!("==> {}", String::from_utf8_lossy(&buf));
}

#[secgate::secure_gate]
pub fn pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    do_pager_start(q1, q2)
}

#[secgate::secure_gate]
pub fn full_object_sync(id: ObjID) {
    let task = EXECUTOR.get().unwrap().spawn(async move {
        let pager = PAGER_DATA.get().unwrap();
        pager.0.sync(&pager.1, id).await
    });
    block_on(EXECUTOR.get().unwrap().run(async { task.await }));
}

#[secgate::secure_gate]
pub fn show_lethe() {
    object_store::with_khf(|khf| {
        println!("{:#?}", khf);
    });
}

#[secgate::secure_gate]
pub fn adv_lethe() {
    object_store::advance_epoch().unwrap();
}
