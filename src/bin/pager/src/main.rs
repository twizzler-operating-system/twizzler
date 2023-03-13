use std::time::Duration;

use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

async fn handle_request(_request: RequestFromKernel) -> Option<CompletionToKernel> {
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

    let num_threads = 1;
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
