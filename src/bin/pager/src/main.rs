use std::{
    collections::BTreeMap,
    error::Error,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use async_executor::{Executor, Task};
use async_io::Timer;
use futures::executor::block_on;
use tickv::{success_codes::SuccessCode, ErrorCode};
use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCommand, KernelCompletionData, ObjectRange,
    PagerCompletionData, PhysRange, RequestFromKernel, RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use crate::{
    data::PagerData,
    helpers::{physrange_to_pages, PAGE},
    request_handle::handle_kernel_request,
    store::{Key, KeyValueStore},
};

mod data;
mod helpers;
mod nvme;
mod request_handle;
mod store;

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
    id_str: &str,
    queue_constructor: impl FnOnce(twizzler_queue::Queue<T, U>) -> Q,
) -> Result<Q, String> {
    tracing::info!("Pager Attaching Queue: {}", id_str);

    // Parse the ID from the string
    let id = id_str.parse::<u128>().unwrap();
    // Initialize the object
    let obj_id = ObjID::new(id);
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

fn queue_args(i: usize) -> String {
    return std::env::args().nth(i).unwrap();
}

fn queue_init() -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
) {
    let rq = attach_queue::<RequestFromKernel, CompletionToKernel, _>(
        &queue_args(1),
        twizzler_queue::CallbackQueueReceiver::new,
    )
    .unwrap();
    let sq = attach_queue::<RequestFromPager, CompletionToPager, _>(
        &queue_args(2),
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
        tracing::info!("submitting request to kernel");

        let res = sq.submit_and_wait(RequestFromPager::new(
            twizzler_abi::pager::PagerRequest::EchoReq,
        ));
        let x = res.await;
        tracing::info!(" got {:?} in response", x);
        timeout.await;
    }));

    Ok(())
}

fn verify_health(health: Result<(), String>) {
    match health {
        Ok(()) => tracing::info!("health check successful"),
        Err(_) => tracing::info!("gealth check failed"),
    }
}

/***
 * Pager Initialization generic function which calls specific initialization functions
 */
fn pager_init() -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    PagerData,
    &'static Executor<'static>,
) {
    tracing::info!("init start");
    tracing_init();
    let data = data_structure_init();
    let (rq, sq) = queue_init();
    let ex = async_runtime_init(2);

    let health = health_check(&rq, &sq, ex, None);
    verify_health(health.clone());
    drop(health);

    tracing::info!("init complete");
    return (rq, sq, data, ex);
}

fn spawn_queues(
    rq: twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    data: PagerData,
    ex: &'static Executor<'static>,
) {
    tracing::info!("spawning queues...");
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
    tracing::info!("queue receiving...");
    let q = Arc::new(q);
    let data = Arc::new(data);
    loop {
        let (id, request) = q.receive().await.unwrap();
        tracing::info!("got request: ({},{:?})", id, request);

        let qc = Arc::clone(&q);
        let datac = Arc::clone(&data);
        ex.spawn(async move {
            let comp = handler(request, datac).await;
            notify(qc, id, comp).await;
        })
        .detach();
    }
}

async fn notify<R, C>(q: Arc<twizzler_queue::CallbackQueueReceiver<R, C>>, id: u32, res: Option<C>)
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    if let Some(res) = res {
        q.complete(id, res).await.unwrap();
    }
    tracing::info!("request {} complete", id);
}

async fn send_request<R, C>(
    q: Arc<twizzler_queue::QueueSender<R, C>>,
    request: R,
) -> Result<C, Box<dyn std::error::Error>>
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    tracing::info!("submitting request {:?}", request);
    return q
        .submit_and_wait(request)
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>);
}

async fn report_ready(
    q: Arc<twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>>,
    ex: &'static Executor<'static>,
) -> Option<PagerCompletionData> {
    tracing::info!("sending ready signal to kernel");
    let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::Ready);

    match send_request(q, request).await {
        Ok(completion) => {
            tracing::info!("received completion for ready signal: {:?}", completion);
            return Some(completion.data());
        }
        Err(e) => {
            tracing::debug!("error from ready signal {:?}", e);
            return None;
        }
    }
}

fn main() {
    let (rq, sq, data, ex) = pager_init();
    spawn_queues(rq, data.clone(), ex);
    let sq = Arc::new(sq);
    let sqc = Arc::clone(&sq);

    let phys_range: Option<PhysRange> = block_on(async move {
        let res = report_ready(sqc, ex).await;
        match res {
            Some(PagerCompletionData::DramPages(range)) => {
                Some(range) // Return the range
            }
            _ => {
                tracing::debug!("ERROR: no range from ready request");
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
        tracing::info!("cannot complete pager initialization with no physical memory");
    }

    tracing::info!("Performing Test...");
    let sqc = Arc::clone(&sq);
    block_on(async move {
        let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::TestReq);
        let _ = send_request(sqc, request).await.ok();
    });

    tracing::info!("Test Completed");
    //Done
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Foo {
    x: u32,
}

static mut RAND_STATE: u32 = 0;
pub fn quick_random() -> u32 {
    let state = unsafe { RAND_STATE };
    let newstate = state.wrapping_mul(69069).wrapping_add(5);
    unsafe {
        RAND_STATE = newstate;
    }
    newstate >> 16
}

struct Tester<'a> {
    kv: KeyValueStore<'a>,
    truth: BTreeMap<Key, Foo>,
}

#[allow(dead_code)]
impl<'a> Tester<'a> {
    fn test(&mut self) {
        const TEST_ITERS: u32 = 100000;
        for i in 0..TEST_ITERS {
            // Every once in a while, validate some things.
            if i % 2000 == 0 {
                self.validate_has_all();
            }
            let x = i % (10001 + i / 1000);
            let k = Key::new(ObjID::new(0), x, store::KeyKind::ObjectInfo);
            let _ = self.get(k);
            let num = quick_random() % 3;
            if num == 0 || num == 2 {
                let _ = self.put(k, Foo { x });
            } else if num == 1 {
                let _ = self.del(k);
            }
        }
    }

    fn validate_has_all(&self) {
        for (key, val) in self.truth.iter() {
            let res: Foo = self.kv.get(*key).unwrap();
            assert_eq!(res, *val);
        }
    }

    fn get(&self, key: Key) -> Result<Foo, ErrorCode> {
        let r = self.kv.get(key);
        if r.is_ok() {
            assert!(self.truth.contains_key(&key));
            let t = self.truth.get(&key).unwrap();
            assert_eq!(t, r.as_ref().unwrap());
        } else {
            assert!(!self.truth.contains_key(&key));
        }
        r
    }

    fn put(&mut self, key: Key, v: Foo) -> Result<SuccessCode, ErrorCode> {
        let res = self.kv.put(key, v);
        if res.is_ok() {
            assert!(!self.truth.contains_key(&key));
            self.truth.insert(key, v);
        } else {
            assert!(self.truth.contains_key(&key));
        }
        res
    }

    fn del(&mut self, key: Key) -> Result<SuccessCode, ErrorCode> {
        let res = self.kv.del(key);
        if res.is_err() {
            assert!(!self.truth.contains_key(&key));
        } else {
            self.truth.remove(&key).unwrap();
        }
        res
    }
}
