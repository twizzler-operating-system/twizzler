use twizzler_abi::{
    object::ObjID,
    pager::{KernelCompletion, KernelRequest, PagerCompletion, PagerRequest},
};
use twizzler_object::{Object, ObjectInitFlags, Protections};
use twizzler_queue::{CallbackQueueReceiver, Queue};

fn main() {
    let q1id = std::env::var("PAGERQ1OBJ").expect("failed to get kernel request queue ID");
    let q2id = std::env::var("PAGERQ2OBJ").expect("failed to get pager request queue ID");
    let q1id = q1id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", q1id));
    let q1id = ObjID::new(q1id);
    let q2id = q2id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", q2id));
    let q2id = ObjID::new(q2id);
    println!("Hello, world from pager! {} {}", q1id, q2id);

    let kqo = Object::init_id(
        q1id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let pqo = Object::init_id(
        q2id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();

    let kernel_queue =
        CallbackQueueReceiver::new(Queue::<KernelRequest, KernelCompletion>::from(kqo));
    let pager_queue =
        twizzler_queue::QueueSender::new(Queue::<PagerRequest, PagerCompletion>::from(pqo));

    /*
    let pager_q = Queue::<PagerRequest, PagerCompletion>::from(pqo);
    pager_q
        .submit(0, PagerRequest::Ping, SubmissionFlags::empty())
        .unwrap();
    let c = pager_q.get_completion(ReceiveFlags::empty());
    println!("GOT {:?}", c);
    */

    std::thread::spawn(|| {
        twizzler_async::run(async move {
            let c = pager_queue.submit_and_wait(PagerRequest::Ping).await;
            println!("Got {:?}", c);
        });
    });

    println!("pager waiting in handler loop");
    twizzler_async::block_on(async {
        loop {
            kernel_queue
                .handle(|info, req| async move {
                    println!("got kreq: {} {:?}", info, req);
                    KernelCompletion::Ok
                })
                .await
                .unwrap();
        }
    });
}
