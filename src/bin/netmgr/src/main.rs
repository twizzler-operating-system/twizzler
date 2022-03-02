#![feature(thread_local)]
#![feature(async_closure)]
#![feature(asm)]
use std::{
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
};

use twizzler::object::Object;
use twizzler_abi::syscall::LifetimeType;
use twizzler_async::Task;
use twizzler_queue::{CallbackQueueReceiver, Queue, QueueSender, ReceiveFlags, SubmissionFlags};
mod arp;
/*
use twizzler::object::ObjID;
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_async::{Async, AsyncSetup, Task};
use twizzler_queue_raw::{QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

async fn get7() -> i32 {
    println!("hello from async");
    4 + 3
}

fn wait(x: &AtomicU64, v: u64) {
    println!("wait");
    let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
        ThreadSyncReference::Virtual(x as *const AtomicU64),
        v,
        ThreadSyncOp::Equal,
        ThreadSyncFlags::empty(),
    ));
    let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    /*
    while x.load(Ordering::SeqCst) == v {
        core::hint::spin_loop();
    }
    */
}

fn wake(x: &AtomicU64) {
    println!("wake");
    let op = ThreadSync::new_wake(ThreadSyncWake::new(
        ThreadSyncReference::Virtual(x as *const AtomicU64),
        usize::MAX,
    ));
    let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
}

struct Queue<T>(Arc<RawQueue<T>>);

impl<T: Copy + Default> AsyncSetup for Queue<T> {
    type Error = twizzler_queue_raw::QueueError;
    const WOULD_BLOCK: Self::Error = Self::Error::WouldBlock;

    fn setup_sleep(&self) -> ThreadSyncSleep {
        let (ptr, val) = self.0.setup_sleep_simple();
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(ptr as *const AtomicU64),
            val,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

fn it_transmits() {
    println!("queue test");
    let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
    let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
    let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

    let q = Arc::new(q);
    let queue = Queue(q.clone());

    let aq = Async::new(queue);

    let t = Task::spawn(async move {
        let mut i = 0;
        loop {
            let res = aq
                .run_with(|q| {
                    println!("q rec {:?}", std::thread::current().id());
                    let x = q.0.receive(wait, wake, ReceiveFlags::NON_BLOCK);
                    println!("internal rec got {:?}", x);
                    x
                })
                .await;
            i += 1;
            println!("rec got {} {:?}", i, res);
        }
    });
    println!("aq spawned");
    for i in 0..1000 {
        let res = q.submit(
            QueueEntry::new(i as u32, i * 10),
            wait,
            wake,
            SubmissionFlags::empty(),
        );
    }
    loop {}

    /*

    for i in 0..100 {
        let res = q.submit(
            QueueEntry::new(i as u32, i * 10),
            wait,
            wake,
            SubmissionFlags::empty(),
        );
        assert_eq!(res, Ok(()));
        let res = q.receive(wait, wake, ReceiveFlags::empty());
        assert!(res.is_ok());
        assert_eq!(res.unwrap().info(), i as u32);
        assert_eq!(res.unwrap().item(), i * 10);
    }
    */
}

fn test_async() {
    println!("main thread id {:?} ", std::thread::current().id(),);
    let res = twizzler_async::block_on(get7());
    println!("async_block: {}", res);

    let res = twizzler_async::run(get7());
    println!("async_run: {}", res);

    let num_threads = 3;
    for _ in 0..num_threads {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    let res = twizzler_async::block_on(async {
        let mut total = 0;
        let mut tasks = vec![];
        for i in 0..100 {
            let x = twizzler_async::Task::spawn(async move {
                let x = get7().await;
                let timer = twizzler_async::Timer::after(Duration::from_millis(100)).await;
                x
            });
            tasks.push(x);
        }
        for (i, t) in tasks.into_iter().enumerate() {
            total += t.await;
        }
        total
    });
    println!("async_thread_pool: {}", res);

    it_transmits();
}
*/

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct Foo {
    x: u32,
    y: u32,
}

fn test_queue() {
    let create = twizzler::object::CreateSpec::new(
        LifetimeType::Volatile,
        twizzler_abi::syscall::BackingType::Normal,
    );
    let queue = Queue::<Foo, Foo>::create(&create, 64, 64).unwrap();
    let obj = queue.object().clone();
    let cbq = Arc::new(CallbackQueueReceiver::new(queue));
    let sq = QueueSender::new(obj.into());

    let num_threads = 3;
    for _ in 0..num_threads {
        // std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    Task::spawn(async move {
        let mut i = 0;
        loop {
            let r = cbq
                .handle(async move |x, mut y| {
                    println!("handle: {} {:?}", x, y);
                    if i % 10 == 0 {
                        twizzler_async::Timer::after(Duration::from_millis(100)).await;
                    }
                    if i % 3 == 0 {
                        twizzler_async::Timer::after(Duration::from_millis(1)).await;
                    }
                    if i % 5 == 0 {
                        twizzler_async::Timer::after(Duration::from_millis(10)).await;
                    }
                    if i % 101 == 0 && i > 0 {
                        twizzler_async::Timer::after(Duration::from_millis(1000)).await;
                    }
                    y.y += 1;
                    y
                })
                .await;
            i += 1;
            println!("loop end {:?}", r);
        }
    })
    .detach();

    let res = twizzler_async::run(async {
        loop {
            let reply = sq.submit_and_wait(Foo { x: 123, y: 456 }).await;
            println!("reply: {:?}", reply);
            reply.unwrap();
        }
    });

    loop {}

    /*
    std::thread::spawn(move || loop {
        let (id, foo) = queue1.receive(ReceiveFlags::empty()).unwrap();
        println!("got {} {:?}", id, foo);
        let reply = Foo {
            x: foo.x,
            y: foo.y + 1,
        };
        queue1
            .complete(id, reply, SubmissionFlags::empty())
            .unwrap();
    });

    let mut i = 0;
    loop {
        queue2
            .submit(i, Foo { x: 123, y: i }, SubmissionFlags::empty())
            .unwrap();
        let (id, reply) = queue2.get_completion(ReceiveFlags::empty()).unwrap();
        println!("got complete {} {:?}", id, reply);
        assert_eq!(reply.x, 123);
        assert_eq!(reply.y, i + 1);
        assert_eq!(id, i);
        i += 1;
    }

    let res = queue.submit(123, Foo { x: 456, y: 111 }, SubmissionFlags::empty());
    println!("sub res {:?}", res);
    let res = queue.receive(ReceiveFlags::empty());
    println!("rec res {:?}", res);
    let res = queue.complete(123, Foo { x: 456, y: 999 }, SubmissionFlags::empty());
    println!("com res {:?}", res);
    let res = queue.get_completion(ReceiveFlags::empty());
    println!("gcm res {:?}", res);
    */
}

fn main() {
    println!("Hello, world from netmgr!");
    for arg in std::env::args() {
        println!("arg {}", arg);
    }
    //arp::test_arp();
    test_queue();
    loop {}

    if std::env::args().len() < 10 {
        //test_async();
    }
    loop {}
    /*
    for _ in 0..4 {
        std::thread::spawn(|| println!("hello from thread {:?}", std::thread::current().id()));
    }
    let id = std::env::args()
        .nth(1)
        .expect("netmgr needs to know net obj id");
    let id = id
        .parse::<u128>()
        .expect(&format!("failed to parse object ID string {}", id));
    let id = ObjID::new(id);
    println!("setup with {:?}", id);

    loop {
        println!("[netmgr] waiting");
        let o = twizzler_net::server_rendezvous(id);
        println!("[netmgr] got {:?}", o);
    }
    */
}
