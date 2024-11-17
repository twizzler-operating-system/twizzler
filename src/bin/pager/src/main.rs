use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

use async_executor::{Executor, Task};
use async_io::Timer;
use futures::executor::block_on;
use tickv::{success_codes::SuccessCode, ErrorCode};
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use crate::store::{Key, KeyValueStore};

mod nvme;
mod store;

async fn handle_request(_request: RequestFromKernel) -> Option<CompletionToKernel> {
    Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Foo {
    x: u32,
}

extern crate twizzler_runtime;
pub static EXECUTOR: OnceLock<Executor> = OnceLock::new();

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

    let ex = EXECUTOR.get_or_init(|| Executor::new());

    let num_threads = 2;
    for _ in 0..(num_threads - 1) {
        std::thread::spawn(|| block_on(ex.run(std::future::pending::<()>())));
    }

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
        }
    })
    .detach();

    ex.spawn(async move {
        loop {
            let (id, request) = rq.receive().await.unwrap();
            println!(" pager: got req from kernel: {} {:?}", id, request);
            let reply = handle_request(request).await;
            if let Some(reply) = reply {
                rq.complete(id, reply).await.unwrap();
            }
        }
    })
    .detach();
    block_on(ex.run(std::future::pending::<()>()));
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
