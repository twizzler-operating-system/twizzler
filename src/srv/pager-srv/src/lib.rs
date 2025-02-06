#![feature(ptr_sub_ptr)]
#![feature(naked_functions)]

use std::sync::{Arc, Mutex, OnceLock};

use async_executor::Executor;
use async_io::block_on;
use colored::Colorize;
use disk::Disk;
use object_store::{key_fprint, LetheState, ObjectStore};
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjectBuilder, RawObject},
};
use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, PagerCompletionData, PhysRange, RequestFromKernel,
    RequestFromPager,
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use twizzler_queue::QueueSender;

use crate::{data::PagerData, request_handle::handle_kernel_request};

mod data;
mod disk;
mod helpers;
mod nvme;
mod physrw;
mod request_handle;

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
    tracing_log::LogTracer::init().unwrap();
}

/***
 * Pager Data Structures Initialization
 */
fn data_structure_init() -> PagerData {
    let pager_data = PagerData::new();

    return pager_data;
}

/***
 * Queue Initializing
 */
fn attach_queue<T: std::marker::Copy, U: std::marker::Copy, Q>(
    obj_id: ObjID,
    queue_constructor: impl FnOnce(twizzler_queue::Queue<T, U>) -> Q,
) -> Result<Q, String> {
    tracing::debug!("Pager Attaching Queue: {}", obj_id);

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

fn queue_init(
    q1: ObjID,
    q2: ObjID,
) -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
) {
    let rq = attach_queue::<RequestFromKernel, CompletionToKernel, _>(
        q1,
        twizzler_queue::CallbackQueueReceiver::new,
    )
    .unwrap();
    let sq = attach_queue::<RequestFromPager, CompletionToPager, _>(
        q2,
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

    for _ in 0..n {
        std::thread::spawn(|| block_on(ex.run(std::future::pending::<()>())));
    }

    return ex;
}

/***
 * Pager Initialization generic function which calls specific initialization functions
 */
fn pager_init(
    q1: ObjID,
    q2: ObjID,
) -> (
    twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    twizzler_queue::QueueSender<RequestFromPager, CompletionToPager>,
    PagerData,
    &'static Executor<'static>,
) {
    tracing_init();
    tracing::debug!("init start");
    let data = data_structure_init();
    let (rq, sq) = queue_init(q1, q2);
    let ex = async_runtime_init(4);

    tracing::debug!("init complete");
    return (rq, sq, data, ex);
}

fn spawn_queues(
    ctx: &'static PagerContext,
    kernel_rq: twizzler_queue::CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
    ex: &'static Executor<'static>,
) {
    tracing::debug!("spawning queues...");
    ex.spawn(listen_queue(kernel_rq, ctx, handle_kernel_request, ex))
        .detach();
}

async fn listen_queue<R, C, F>(
    kernel_rq: twizzler_queue::CallbackQueueReceiver<R, C>,
    ctx: &'static PagerContext,
    handler: impl Fn(&'static PagerContext, R) -> F + Copy + Send + Sync + 'static,
    ex: &'static Executor<'static>,
) where
    F: std::future::Future<Output = Option<C>> + Send + 'static,
    R: std::fmt::Debug + Copy + Send + Sync + 'static,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    let q = Arc::new(kernel_rq);
    loop {
        tracing::trace!("queue receiving...");
        let (id, request) = q.receive().await.unwrap();
        tracing::trace!("got request: ({},{:?})", id, request);

        let qc = Arc::clone(&q);
        ex.spawn(async move {
            let comp = handler(ctx, request).await;
            notify(&qc, id, comp).await;
        })
        .detach();
    }
}

async fn notify<R, C>(q: &Arc<twizzler_queue::CallbackQueueReceiver<R, C>>, id: u32, res: Option<C>)
where
    R: std::fmt::Debug + Copy + Send + Sync,
    C: std::fmt::Debug + Copy + Send + Sync + 'static,
{
    if let Some(res) = res {
        q.complete(id, res).await.unwrap();
    }
    tracing::trace!("request {} complete", id);
}

async fn report_ready(
    ctx: &PagerContext,
    _ex: &'static Executor<'static>,
) -> Option<PagerCompletionData> {
    tracing::debug!("sending ready signal to kernel");
    let request = RequestFromPager::new(twizzler_abi::pager::PagerRequest::Ready);

    match ctx.sender.submit_and_wait(request).await {
        Ok(completion) => {
            tracing::debug!("received completion for ready signal: {:?}", completion);
            return Some(completion.data());
        }
        Err(e) => {
            tracing::warn!("error from ready signal {:?}", e);
            return None;
        }
    }
}

struct PagerContext {
    data: PagerData,
    sender: Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    ostore: ObjectStore<disk::Disk>,
}

static PAGER_CTX: OnceLock<PagerContext> = OnceLock::new();

fn do_pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    let (rq, sq, data, ex) = pager_init(q1, q2);
    let disk = block_on(ex.run(Disk::new(ex))).unwrap();
    let ostore = object_store::ObjectStore::open(disk, [0; 32]);
    let sq = Arc::new(sq);
    let sqc = sq.clone();
    let _ = PAGER_CTX.set(PagerContext {
        data,
        sender: sq,
        ostore,
    });
    let ctx = PAGER_CTX.get().unwrap();
    spawn_queues(ctx, rq, ex);

    block_on(ex.run(async move {
        let res = report_ready(&ctx, ex).await;
    }));
    tracing::info!("pager ready");

    let bootstrap_id = ctx.ostore.get_config_id().unwrap().unwrap_or_else(|| {
        tracing::info!("creating new naming object");
        let vo = VecObject::<u32, VecObjectAlloc>::new(ObjectBuilder::default().persist()).unwrap();
        ctx.ostore.set_config_id(vo.object().id().raw()).unwrap();
        vo.object().id().raw()
    });
    tracing::info!("found root namespace: {:x}", bootstrap_id);

    return bootstrap_id.into();

    /*
    object_store::create_object(17).unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("newobj {:#?}", khf);
    });
    object_store::write_all(17, b"this is a test", 0).unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("written {:#?}", khf);
    });

    object_store::advance_epoch().unwrap();
    object_store::with_khf(|khf| {
        tracing::info!("written-adv {:#?}", khf);
    });

    object_store::unlink_object(17).unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("removed {:#?}", khf);
    });
    object_store::advance_epoch().unwrap();

    object_store::with_khf(|khf| {
        tracing::info!("removed-adv {:#?}", khf);
    });

    loop {}
    let mut buf = [0; 12];
    object_store::read_exact(0x5d74fb7c3fe55e64131351157f1fd996u128, &mut buf, 0).unwrap();
    println!("==> {}", String::from_utf8_lossy(&buf));
    object_store::advance_epoch().unwrap();
    object_store::read_exact(17, &mut buf, 0).unwrap();
    println!("==> {}", String::from_utf8_lossy(&buf));
    */
}

#[secgate::secure_gate]
pub fn pager_start(q1: ObjID, q2: ObjID) -> ObjID {
    do_pager_start(q1, q2)
}

#[secgate::secure_gate]
pub fn full_object_sync(id: ObjID) {
    let task = EXECUTOR.get().unwrap().spawn(async move {
        let pager = PAGER_CTX.get().unwrap();
        pager.data.sync(&pager, id).await
    });
    block_on(EXECUTOR.get().unwrap().run(async { task.await }));
}

#[secgate::secure_gate]
pub fn show_lethe() {
    colored::control::set_override(true);
    static LAST: Mutex<Option<LetheState>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let state = PAGER_CTX.get().unwrap().ostore.get_lethe_state().unwrap();
    for po in &state.list {
        println!("{}", po);
    }
    for root in &state.roots {
        if let Some(last) = &*last {
            let item = last.roots.iter().find(|x| x.0 == root.0 && x.1 == root.1);
            if let Some(item) = item {
                if root.2 != item.2 {
                    println!(
                        " ({}, {}) -- {} -> {}",
                        root.0,
                        root.1,
                        format!("{:8x}", key_fprint(&item.2)).blue(),
                        format!("{:8x}", key_fprint(&root.2)).blue(),
                    );
                } else {
                    println!(" ({}, {}) -- {:8x}", root.0, root.1, key_fprint(&root.2));
                }
            } else {
                println!(
                    " ({}, {}) -- {} {}",
                    root.0,
                    root.1,
                    format!("{:8x}", key_fprint(&root.2)).green(),
                    "[new]".green(),
                );
            }
        } else {
            println!(" ({}, {}) -- {:8x}", root.0, root.1, key_fprint(&root.2));
        }
    }
    if let Some(last) = &*last {
        for root in &last.roots {
            let item = state.roots.iter().find(|x| x.0 == root.0 && x.1 == root.1);
            if item.is_none() {
                println!(
                    " ({}, {}) -- {} {}",
                    root.0,
                    root.1,
                    format!("{:8x}", key_fprint(&root.2)).red(),
                    "[deleted]".red()
                );
            }
        }
    }

    *last = Some(state);
}

#[secgate::secure_gate]
pub fn adv_lethe() {
    PAGER_CTX.get().unwrap().ostore.advance_epoch().unwrap();
}
