#![feature(int_log)]
use std::time::Duration;

use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use std::hint::black_box;

use crate::store::{KeyValueStore, Storage, BLOCK_SIZE};

mod nvme;
mod store;

async fn handle_request(request: RequestFromKernel) -> Option<CompletionToKernel> {
    Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
}

fn main() {
    let idstr = std::env::args().nth(1).unwrap();
    let kidstr = std::env::args().nth(2).unwrap();
    println!("Hello, world from pager: {} {}", idstr, kidstr);
    let id = idstr.parse::<u128>().unwrap();
    let kid = kidstr.parse::<u128>().unwrap();

    let id = ObjID::new(id);
    let kid = ObjID::new(kid);
    let object = Object::init_id(
        id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();

    let kobject = Object::init_id(
        kid,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();

    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::from(object);
    let rq = twizzler_queue::CallbackQueueReceiver::new(queue);

    let kqueue = twizzler_queue::Queue::<RequestFromPager, CompletionToPager>::from(kobject);
    let sq = twizzler_queue::QueueSender::new(kqueue);

    let num_threads = 2;
    for _ in 0..(num_threads - 1) {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    twizzler_async::Task::spawn(async move {
        loop {
            let timeout = twizzler_async::Timer::after(Duration::from_millis(1000));
            println!("pager submitting request");
            let res = sq.submit_and_wait(RequestFromPager::new(
                twizzler_abi::pager::PagerRequest::EchoReq,
            ));
            let x = res.await;
            println!("pager got {:?} in response", x);
            timeout.await;
            break;
        }
    })
    .detach();
    let nvme_ctrl = twizzler_async::block_on(nvme::init_nvme());
    println!("a :: {}", twizzler_async::block_on(nvme_ctrl.flash_len()));
    let storage = Storage::new(nvme_ctrl);
    let mut read_buffer = [0; BLOCK_SIZE];
    let kv = KeyValueStore::new(storage, &mut read_buffer, 4096 * 1000);
    let kv = black_box(kv).unwrap();
    let mut buf = [0; BLOCK_SIZE];
    println!(":: {:?}", kv.get(1, &mut buf));

    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::from(object);
    let rq = twizzler_queue::CallbackQueueReceiver::new(queue);

    twizzler_async::Task::spawn(async move {
        loop {
            let (id, request) = rq.receive().await.unwrap();
            println!("got req from kernel: {} {:?}", id, request);
            let reply = handle_request(request).await;
            if let Some(reply) = reply {
                rq.complete(id, reply).await.unwrap();
            }
        }
    })
    .detach();
    twizzler_async::run(std::future::pending::<()>());
}
