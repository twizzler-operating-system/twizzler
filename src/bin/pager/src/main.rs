use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

use async_executor::{Executor, Task};
use async_io::Timer;
use futures::executor::block_on;
use tickv::{success_codes::SuccessCode, ErrorCode};
/*
use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
*/
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use crate::store::{Key, KeyValueStore};

mod nvme;
mod store;

pub static EXECUTOR: OnceLock<Executor> = OnceLock::new();

/***
 * Tracing Init
 ***/
/*
fn tracing_init() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .finish(),
    ).unwrap();
}
*/

/*** 
 * Queue Initializing
 */
fn attach_queue<T: std::marker::Copy, U: std::marker::Copy, Q>(
    id_str: &str,
    queue_constructor: impl FnOnce(twizzler_queue::Queue<T, U>) -> Q,
) -> Result<Q, String> {
    println!("Pager Attaching Queue: {}", id_str);

    // Parse the ID from the string
    let id = id_str.parse::<u128>().unwrap();
    // Initialize the object
    let obj_id = ObjID::new(id);
    let object = Object::init_id(
        obj_id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    ).unwrap();

    // Ensure the object is cast or transformed to match the expected `Queue` type
    let queue: twizzler_queue::Queue<T, U> = twizzler_queue::Queue::from(object);
    
    Ok(queue_constructor(queue))
}

fn queue_args(i: usize) -> String {
    return std::env::args().nth(i).unwrap();
}

fn queue_init() -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>, 
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>
    ) {
    println!("Hello, world from pager!");

    let rq = attach_queue::<RequestFromKernel, CompletionToKernel, _>(&queue_args(1), twizzler_queue::CallbackQueueReceiver::new).unwrap();
    let sq = attach_queue::<RequestFromPager, CompletionToPager, _>(&queue_args(2), twizzler_queue::QueueSender::new).unwrap();

    return (rq, sq);
}

/*** 
 * Async Runtime Initialization
 * Creating n threads
 ***/
fn async_runtime_init(n: i32) -> &'static Executor<'static> {
    let ex = EXECUTOR.get_or_init(|| Executor::new());

    for _ in 0..(n - 1) {
        std::thread::spawn(|| block_on(ex.run(std::future::pending::<()>())));
    }

    return ex;
}

/***
 * Health Check
 ***/
fn health_check(
    _rq: &twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>, 
    sq: &twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    ex: &'static Executor<'static>,
    timeout_ms: Option<u64>
    ) -> Result<(), String> {
    let timeout_duration = Duration::from_millis(timeout_ms.unwrap_or(1000) as u64);

    println!("Beginning Pager Health Check...");
    block_on(ex.run(
            async move{
                let timeout = Timer::after(timeout_duration);
                println!("-- pager: submitting request on P2K Queue");

                let res = sq.submit_and_wait(RequestFromPager::new(
                        twizzler_abi::pager::PagerRequest::EchoReq,
                        ));
                let x = res.await;
                println!(" pager:  got {:?} in response", x);
                timeout.await;
    }));

    Ok(())
}

fn verify_health(health: Result<(), String>) {
    match health {
        Ok(()) => println!("Health Check Successful"),
        Err(_) => println!("Health Check Unsuccessful")
    }
}

/***
 * Pager Initialization generic function which calls specific initialization functions 
 ***/
fn pager_init() -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>, 
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    &'static Executor<'static>
    ) {
    
    //tracing_init();
    //Data Structure Initialization
    let (rq, sq) = queue_init();
    let ex = async_runtime_init(2);

    let health = health_check(&rq, &sq, ex, None);
    verify_health(health.clone());
    drop(health);

    return (rq, sq, ex);
}

fn spawn_queues(
    rq: twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>, 
    sq: twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    ex: &'static Executor<'static>
) {
    println!("Spawning Queues...");
    ex.spawn(listen_queue(rq, handle_kernel_request, ex)).detach();
}

async fn listen_queue<R, C, F>(
    q: twizzler_queue::CallbackQueueReceiver<R, C>,
    handler: impl Fn(R) -> F + Copy + Send + Sync + 'static,
    ex: &'static Executor<'static>
    ) 
    where 
    F: std::future::Future<Output = Option<C>> + Send + 'static,
    R: std::fmt::Debug + Copy + Send + Sync + 'static,
    C: std::fmt::Debug + Copy + Send + Sync + 'static
    {
        println!("-- pager: Queue Up");
        loop {
            let (id, request) = q.receive().await.unwrap(); 
            println!("-- pager: got request from kernel: ({},{:?})", id, request);
            ex.spawn(
                async move{
                    let comp = handler(request).await;
                    notify(&q, id, comp).await;
                }
                ).detach(); 
        }
}

async fn notify<R, C>(q: &twizzler_queue::CallbackQueueReceiver<R, C>, id: u32, res: Option<C>)
    where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static
{
    if let Some(res) = res {
        q.complete(id, res).await.unwrap();
    }
}

async fn handle_kernel_request(request: RequestFromKernel) -> Option<CompletionToKernel> {
    println!("Handling Kernel Request {:?}", request);
    Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
}

fn main() {
    let (rq, sq, ex) = pager_init();
    spawn_queues(rq, sq, ex);
    //Spawn listening queues
    //Submit ready to kernel
    //Return
    //
/*
    ex.spawn(async move {
        loop {
            let timeout = Timer::after(Duration::from_millis(1000));
            println!(" pager:  submitting request");
            let res = sq.submit_and_wait(RequestFromPager::new(
                twizzler_abi::pager::PagerRequest::EchoReq,
            ));
            let x = res.await;
            println!(" pager:  got {:?} in response", x);
            timeout.await;
            // TODO: do some other stuff?
            std::future::pending::<()>().await;
        }
    })
    .detach();
    ex.spawn(async move {
        loop {
            let (id, request) = rq.receive().await.unwrap();
            println!(" pager: got req from kernel: {} {:?}", id, request);
            let reply = handle_kernel_request(request).await;
            if let Some(reply) = reply {
                rq.complete(id, reply).await.unwrap();
            }
        }
    })
    .detach();
*/
    block_on(ex.run(std::future::pending::<()>()));

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
