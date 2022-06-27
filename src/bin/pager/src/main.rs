#![feature(once_cell)]
#![feature(option_result_unwrap_unchecked)]
use twizzler_abi::{object::ObjID, pager::KernelCompletion};

use crate::ctx::PagerContext;

mod ctx;
mod device;
mod memory;
mod nvme;

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
    println!("pager starting with queues {} {}", q1id, q2id);

    let ctx = PagerContext::new(q1id, q2id).expect("failed to create pager context");

    twizzler_async::run(async move {
        loop {
            ctx.handle_kernel_req(|_, req| async move {
                println!("got {:?}", req);
                KernelCompletion::Ok
            })
            .await
            .unwrap();
        }
    });
}
