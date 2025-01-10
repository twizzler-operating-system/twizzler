#![feature(ptr_sub_ptr)]
#![feature(naked_functions)]

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use async_executor::Executor;
use async_io::Timer;
use futures::executor::block_on;
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, PagerCompletionData, PhysRange, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

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
            .with_max_level(tracing::Level::INFO)
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

    for _ in 0..(n - 1) {
        std::thread::spawn(|| block_on(ex.run(std::future::pending::<()>())));
    }

    return ex;
}

/***
 * Health Check
 */
fn health_check(
    _rq: &twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    sq: &twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    ex: &'static Executor<'static>,
    timeout_ms: Option<u64>,
) -> Result<(), String> {
    let timeout_duration = Duration::from_millis(timeout_ms.unwrap_or(1000) as u64);

    tracing::info!("pager health check start...");
    block_on(ex.run(async move {
        let timeout = Timer::after(timeout_duration);
        tracing::debug!("submitting request to kernel");

        let res = sq.submit_and_wait(RequestFromPager::new(
            twizzler_abi::pager::PagerRequest::EchoReq,
        ));
        let x = res.await;
        tracing::debug!(" got {:?} in response", x);
        timeout.await;
    }));

    Ok(())
}

fn verify_health(health: Result<(), String>) {
    match health {
        Ok(()) => tracing::info!("health check successful"),
        Err(_) => tracing::info!("health check failed"),
    }
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
    tracing::debug!("init start");
    tracing_init();
    let data = data_structure_init();
    let (rq, sq) = queue_init(q1, q2);
    let ex = async_runtime_init(2);

    let health = health_check(&rq, &sq, ex, None);
    verify_health(health.clone());
    drop(health);

    tracing::debug!("init complete");
    return (rq, sq, data, ex);
}

fn spawn_queues(
    rq: twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    data: PagerData,
    ex: &'static Executor<'static>,
) {
    tracing::debug!("spawning queues...");
    ex.spawn(listen_queue(rq, data, handle_kernel_request, ex))
        .detach();
}

async fn listen_queue<R, C, F>(
    q: twizzler_queue::CallbackQueueReceiver<R, C>,
    data: PagerData,
    handler: impl Fn(R, Arc<PagerData>) -> F + Copy + Send + Sync + 'static,
    ex: &'static Executor<'static>,
) where
    F: std::future::Future<Output = Option<C>> + Send + 'static,
    R: std::fmt::Debug + Copy + Send + Sync + 'static,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    tracing::debug!("queue receiving...");
    let q = Arc::new(q);
    let data = Arc::new(data);
    loop {
        let (id, request) = q.receive().await.unwrap();
        tracing::trace!("got request: ({},{:?})", id, request);

        let qc = Arc::clone(&q);
        let datac = Arc::clone(&data);
        ex.spawn(async move {
            let comp = handler(request, datac).await;
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

async fn send_request<R, C>(
    q: &Arc<twizzler_queue::QueueSender<R, C>>,
    request: R,
) -> Result<C, Box<dyn std::error::Error>>
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    tracing::trace!("submitting request {:?}", request);
    return q
        .submit_and_wait(request)
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>);
}

async fn report_ready(
    q: &Arc<twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>>,
    _ex: &'static Executor<'static>,
) -> Option<PagerCompletionData> {
    tracing::debug!("sending ready signal to kernel");
    let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::Ready);

    match send_request(q, request).await {
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

fn do_pager_start(q1: ObjID, q2: ObjID) {
    let (rq, sq, data, ex) = pager_init(q1, q2);
    spawn_queues(rq, data.clone(), ex);
    let sq = Arc::new(sq);
    let sqc = Arc::clone(&sq);

    let phys_range: Option<PhysRange> = block_on(async move {
        let res = report_ready(&sq, ex).await;
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
        memory_init(data, range);
    } else {
        tracing::error!("cannot complete pager initialization with no physical memory");
    }

    tracing::info!("Performing Test...");
    let sqc2 = sqc.clone();
    block_on(async move {
        let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::TestReq);
        let _ = send_request(&sqc, request).await.ok();
    });

    if let Some(phys_range) = phys_range {
        block_on(async move {
            let mut buf = vec![0u8; 4096];
            for (i, b) in (&mut buf).iter_mut().enumerate() {
                *b = i as u8;
            }
            let mut buf2 = vec![0u8; 4096];
            tracing::debug!("testing physrw: {:?}", &buf[0..10]);
            assert_ne!(buf, buf2);
            let start = phys_range.start;
            let phys = PhysRange {
                start,
                end: start + buf.len() as u64,
            };
            tracing::debug!("filling physical pages: {:?} from {:p}", phys, buf.as_ptr());
            physrw::fill_physical_pages(&sqc2, buf.as_slice(), phys)
                .await
                .unwrap();

            tracing::debug!(
                "reading physical pages: {:?} into {:p}",
                phys,
                buf2.as_ptr()
            );
            physrw::read_physical_pages(&sqc2, buf2.as_mut_slice(), phys)
                .await
                .unwrap();

            assert_eq!(buf, buf2);
        });
    }

    tracing::info!("Test Completed");
    //Done
}

#[secgate::secure_gate]
pub fn pager_start(q1: ObjID, q2: ObjID) {
    do_pager_start(q1, q2);
}
