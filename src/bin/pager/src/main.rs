#![feature(int_log)]
#![feature(once_cell)]
use std::sync::Arc;

use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, RequestFromKernel, RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use std::collections::BTreeMap;

use tickv::{success_codes::SuccessCode, ErrorCode};

use crate::{
    datamgr::DataMgr,
    kernel::{KernelCommandQueue, PagerRequestQueue},
    memory::DramMgr,
    pager::Pager,
    store::{Key, KeyValueStore, Storage},
};

mod datamgr;
mod kernel;
mod memory;
mod nvme;
mod pager;
mod store;

fn main() {
    let idstr = std::env::args().nth(1).unwrap();
    let kidstr = std::env::args().nth(2).unwrap();
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

    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::from(object);
    let rq = twizzler_queue::CallbackQueueReceiver::new(queue);

    let object = Object::init_id(
        kid,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let queue = twizzler_queue::Queue::<RequestFromPager, CompletionToPager>::from(object);
    let sq = twizzler_queue::QueueSender::new(queue);

    let num_threads = std::thread::available_parallelism().unwrap().get();
    for _ in 0..(num_threads - 1) {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    let nvme_ctrl = twizzler_async::block_on(nvme::init_nvme());
    let len = twizzler_async::block_on(nvme_ctrl.flash_len());
    let storage = Storage::new(nvme_ctrl);

    let pager = Arc::new(Pager::new(
        KernelCommandQueue::new(rq),
        PagerRequestQueue::new(sq),
        DramMgr::default(),
        DataMgr::new(storage, len).unwrap(),
    ));

    let pager_m = pager.clone();
    twizzler_async::Task::spawn(async move {
        loop {
            pager_m.handler_main().await;
        }
    })
    .detach();

    let pager_d = pager.clone();
    twizzler_async::Task::spawn(async move {
        loop {
            pager_d.dram_manager_main().await;
        }
    })
    .detach();
    twizzler_async::run(std::future::pending::<()>());
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

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Foo {
    x: u32,
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
                let _ = self.put(k, Foo { x: x });
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
