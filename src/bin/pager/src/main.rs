use twizzler_abi::pager::{CompletionToKernel, KernelCompletionData, RequestFromKernel};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

async fn handle_request(request: RequestFromKernel) -> Option<CompletionToKernel> {
    Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
}

fn main() {
    let idstr = std::env::args().nth(1).unwrap();
    println!("Hello, world from pager: {}", idstr);
    let id = idstr.parse::<u128>().unwrap();

    let id = ObjID::new(id);
    let object = Object::init_id(
        id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();

    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::from(object);
    let rq = twizzler_queue::CallbackQueueReceiver::new(queue);

    let num_threads = 1;
    for _ in 0..(num_threads - 1) {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }
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
