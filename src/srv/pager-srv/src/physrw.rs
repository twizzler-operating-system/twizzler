use std::{
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, OnceLock,
    },
    thread::JoinHandle,
};

use futures::executor::LocalPool;
use twizzler::object::ObjID;
use twizzler_abi::pager::{CompletionToPager, PagerRequest, PhysRange, RequestFromPager};
use twizzler_queue::QueueSender;
use twizzler_rt_abi::{error::TwzError, Result};

type Queue = QueueSender<RequestFromPager, CompletionToPager>;
type QueueRef = Arc<Queue>;

fn get_object(ptr: *const u8) -> (ObjID, usize) {
    let handle = twizzler_rt_abi::object::twz_rt_get_object_handle(ptr).unwrap();
    (handle.id(), unsafe {
        ptr.offset_from_unsigned(handle.start())
    })
}

#[derive(Default)]
struct Waiter {
    data: async_lock::Mutex<Option<CompletionToPager>>,
    cv: async_condvar_fair::Condvar,
}

#[derive(Clone)]
struct Request {
    req: RequestFromPager,
    waiter: Arc<Waiter>,
}

impl Request {
    fn new(req: RequestFromPager) -> Self {
        Self {
            req,
            waiter: Arc::new(Waiter::default()),
        }
    }

    async fn wait(&self) -> CompletionToPager {
        loop {
            let mut data = self.waiter.data.lock().await;
            if data.is_some() {
                return data.take().unwrap();
            }
            self.waiter.cv.wait_no_relock(data).await;
        }
    }

    async fn finish(&self, comp: CompletionToPager) {
        *self.waiter.data.lock().await = Some(comp);
        self.waiter.cv.notify_all();
    }
}

struct PageRequestMgr {
    _thread: JoinHandle<()>,
    queue: QueueRef,
    sender: Sender<Request>,
}

impl PageRequestMgr {
    pub async fn submit_and_wait(&self, req: RequestFromPager) -> CompletionToPager {
        let req = Request::new(req);
        let waiter = req.clone();
        self.sender.send(req).unwrap();
        waiter.wait().await
    }
}

static PR_MGR: OnceLock<PageRequestMgr> = OnceLock::new();

async fn pr_mgr_thread_main(recv: Receiver<Request>) {
    loop {
        match recv.recv().ok() {
            Some(req) => {
                let comp = PR_MGR.wait().queue.submit_and_wait(req.req).await.unwrap();
                req.finish(comp).await;
            }
            None => break,
        }
    }
}

fn pr_mgr(queue: &QueueRef) -> &'static PageRequestMgr {
    let (sender, recv) = mpsc::channel();
    PR_MGR.get_or_init(|| PageRequestMgr {
        _thread: std::thread::Builder::new()
            .name("pager-requester-thread".to_owned())
            .spawn(|| LocalPool::new().run_until(async { pr_mgr_thread_main(recv).await }))
            .unwrap(),
        sender,
        queue: queue.clone(),
    })
}

pub async fn register_phys(queue: &QueueRef, start: u64, len: u64) -> Result<()> {
    let request = RequestFromPager::new(PagerRequest::RegisterPhys(start, len));
    let comp = pr_mgr(queue).submit_and_wait(request).await;
    match comp.data() {
        twizzler_abi::pager::PagerCompletionData::Okay => Ok(()),
        twizzler_abi::pager::PagerCompletionData::Error(e) => Err(e.error()),
        _ => Err(TwzError::INVALID_ARGUMENT),
    }
}

async fn do_physrw_request(
    queue: &QueueRef,
    target_object: ObjID,
    offset: usize,
    len: usize,
    phys: PhysRange,
    write_phys: bool,
) -> Result<()> {
    let request = RequestFromPager::new(PagerRequest::CopyUserPhys {
        target_object,
        offset,
        len,
        phys,
        write_phys,
    });
    let comp = pr_mgr(queue).submit_and_wait(request).await;
    match comp.data() {
        twizzler_abi::pager::PagerCompletionData::Okay => Ok(()),
        twizzler_abi::pager::PagerCompletionData::Error(e) => Err(e.error()),
        _ => Err(TwzError::INVALID_ARGUMENT),
    }
}

/// Writes phys.len() bytes from the buffer into physical addresses specified in phys. If the
/// supplied buffer is shorter than the physical range, then the remaining bytes in the physical
/// memory are filled with 0.
pub async fn fill_physical_pages(queue: &QueueRef, buf: &[u8], phys: PhysRange) -> Result<()> {
    let obj = get_object(buf.as_ptr());
    do_physrw_request(queue, obj.0, obj.1, buf.len(), phys, true).await
}

/// Reads buf.len() bytes from physical addresses in phys into the buffer. If the supplied physical
/// range is shorter than the buffer, then the remaining bytes in the buffer are filled with 0.
#[allow(dead_code)]
pub async fn read_physical_pages(queue: &QueueRef, buf: &mut [u8], phys: PhysRange) -> Result<()> {
    let obj = get_object(buf.as_ptr());
    do_physrw_request(queue, obj.0, obj.1, buf.len(), phys, false).await
}
