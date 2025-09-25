use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::Waker,
    thread::{available_parallelism, JoinHandle},
    time::Instant,
};

use async_executor::LocalExecutor;
use async_io::block_on;
use twizzler_abi::pager::{CompletionToKernel, KernelCommand, ObjectEvictFlags, RequestFromKernel};
use twizzler_queue::{QueueError, ReceiveFlags, SubmissionFlags};

use crate::{request_handle::handle_kernel_request, PAGER_CTX};

pub struct WorkItem {
    start: Instant,
    qid: u32,
    req: RequestFromKernel,
}

impl WorkItem {
    fn new(qid: u32, req: RequestFromKernel) -> Self {
        Self {
            start: Instant::now(),
            qid,
            req,
        }
    }
}

pub struct WorkerThread {
    _handle: JoinHandle<()>,
    pending: async_channel::Sender<WorkItem>,
}

#[thread_local]
static LOCAL_EXEC: LocalExecutor<'static> = LocalExecutor::new();

impl WorkerThread {
    fn new() -> Self {
        let (send, recv) = async_channel::bounded::<WorkItem>(32);
        Self {
            _handle: std::thread::spawn(move || loop {
                let wi = block_on(LOCAL_EXEC.run(recv.recv())).unwrap();
                tracing::trace!(
                    "{}: starting handling after {}us",
                    wi.qid,
                    wi.start.elapsed().as_micros()
                );

                let resp = run_async(handle_kernel_request(
                    PAGER_CTX.get().unwrap(),
                    wi.qid,
                    wi.req,
                ));
                tracing::trace!(
                    "{}: done handling after {}us",
                    wi.qid,
                    wi.start.elapsed().as_micros()
                );
                for resp in resp {
                    PAGER_CTX
                        .get()
                        .unwrap()
                        .kernel_notify
                        .complete(wi.qid, resp, SubmissionFlags::empty())
                        .unwrap();
                }
            }),
            pending: send,
        }
    }
}

pub struct Workers {
    threads: Vec<WorkerThread>,
}

impl Workers {
    fn new() -> Self {
        let mut threads = Vec::new();
        let nr_threads = (available_parallelism().unwrap().get() / 3).clamp(2, 8);
        for _ in 0..nr_threads {
            threads.push(WorkerThread::new());
        }
        Self { threads }
    }
}

pub struct PagerThreadPool {
    _workers: Arc<Workers>,
    _kq_handler: JoinHandle<()>,
}

impl PagerThreadPool {
    pub fn new(
        queue: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
    ) -> Self {
        let pool = Arc::new(Workers::new());
        PagerThreadPool {
            _workers: pool.clone(),
            _kq_handler: std::thread::spawn(move || kq_handler_main(pool, queue)),
        }
    }
}

pub fn spawn_async<O: 'static>(f: impl Future<Output = O> + 'static) {
    LOCAL_EXEC.spawn(f).detach();
}

pub fn run_async<O: 'static>(f: impl Future<Output = O>) -> O {
    block_on(LOCAL_EXEC.run(f))
}

fn kq_handler_main(
    workers: Arc<Workers>,
    queue: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
) {
    let mut i = 0;
    loop {
        let mut tmp = heapless::Vec::<(u32, RequestFromKernel), 8>::new();
        while !tmp.is_full() {
            let res = queue.receive(ReceiveFlags::NON_BLOCK);
            match res {
                Ok((id, req)) => unsafe { tmp.push_unchecked((id, req)) },
                Err(e) if e == QueueError::WouldBlock => {
                    if !tmp.is_empty() {
                        break;
                    }
                    if let Ok((id, req)) = queue.receive(ReceiveFlags::empty()) {
                        unsafe { tmp.push_unchecked((id, req)) };
                    }
                }
                Err(e) => {
                    tracing::error!("queue recieve error: {}", e);
                }
            }
        }

        for (id, req) in tmp {
            if let KernelCommand::ObjectEvict(evict) = req.cmd() {
                // Send all per-object evict streams to one thread so they preserve order.
                if evict.flags.contains(ObjectEvictFlags::FENCE) {
                    let wi = WorkItem::new(id, req);
                    let idx = req.id().map_or(0, |x| x.parts()[0] ^ x.parts()[1]) + id as u64;
                    workers.threads[(idx as usize) % workers.threads.len()]
                        .pending
                        .try_send(wi)
                        .unwrap();
                } else {
                    let resp = run_async(handle_kernel_request(PAGER_CTX.get().unwrap(), id, req));
                    for resp in resp {
                        PAGER_CTX
                            .get()
                            .unwrap()
                            .kernel_notify
                            .complete(id, resp, SubmissionFlags::empty())
                            .unwrap();
                    }
                }
            } else {
                let wi = WorkItem::new(id, req);
                workers.threads[i % workers.threads.len()]
                    .pending
                    .try_send(wi)
                    .unwrap();
                i += 1;
            }
        }
    }
}

pub struct Waiter<T: Send> {
    data: Mutex<(Option<T>, Option<Waker>)>,
}

impl<T: Send> Default for Waiter<T> {
    fn default() -> Self {
        Self {
            data: Mutex::new((None, None)),
        }
    }
}

impl<T: Send> Waiter<T> {
    pub fn finish(&self, item: T) {
        let mut data = self.data.lock().unwrap();
        data.0 = Some(item);
        if let Some(w) = data.1.take() {
            w.wake();
        }
    }
}

impl<T: Send> Future for &Waiter<T> {
    type Output = T;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut data = self.data.lock().unwrap();
        if data.0.is_some() {
            std::task::Poll::Ready(data.0.take().unwrap())
        } else {
            data.1.replace(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}
