use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    task::{Poll, Waker},
};

use async_io::Async;
use futures::FutureExt;
use twizzler_queue_raw::{QueueError, ReceiveFlags, SubmissionFlags};

use crate::Queue;

struct QueueSenderInner<S, C> {
    queue: Queue<S, C>,
}

struct WaitPoint<C> {
    item: Option<(u32, C)>,
    waker: Option<Waker>,
}

struct WaitPointFuture<'a, S: Copy + Send + Sync, C: Copy + Send + Sync> {
    state: Arc<Mutex<WaitPoint<C>>>,
    sender: &'a QueueSender<S, C>,
}

impl<'a, S: Copy + Send + Sync, C: Copy + Send + Sync> Future for WaitPointFuture<'a, S, C> {
    type Output = Result<(u32, C), QueueError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some((id, item)) = self.sender.poll_completions() {
            self.sender.handle_completion(id, item);
        }
        let mut state = self.state.lock().unwrap();
        if let Some(item) = state.item.take() {
            Poll::Ready(Ok(item))
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// An async-supported sending-half of a [Queue]. This is to support systems that want to
/// asynchronously send items to a receiver, under the assumption that the receiver sends
/// completions to indicate that a request has been finished, and that the send ID can be reused.
///
/// Thus, this queue interally allocates, sends, and reuses IDs for requests.
pub struct QueueSender<S: Copy, C: Copy> {
    counter: AtomicU32,
    reuse: Mutex<Vec<u32>>,
    inner: Async<Pin<Box<QueueSenderInner<S, C>>>>,
    calls: Mutex<BTreeMap<u32, Arc<Mutex<WaitPoint<C>>>>>,
}

impl<S: Copy, C: Copy> twizzler_futures::TwizzlerWaitable for QueueSenderInner<S, C> {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        self.queue.setup_read_com_sleep()
    }

    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        self.queue.setup_write_sub_sleep()
    }
}

impl<S: Copy + Sync + Send, C: Copy + Send + Sync> QueueSender<S, C> {
    /// Build a new QueueSender from a [Queue].
    pub fn new(queue: Queue<S, C>) -> Self {
        Self {
            counter: AtomicU32::new(0),
            reuse: Mutex::new(vec![]),
            inner: Async::new(QueueSenderInner { queue }).unwrap(),
            calls: Mutex::new(BTreeMap::new()),
        }
    }

    fn next_id(&self) -> u32 {
        let mut reuse = self.reuse.lock().unwrap();
        reuse
            .pop()
            .unwrap_or_else(|| self.counter.fetch_add(1, Ordering::SeqCst))
    }

    pub unsafe fn release_id(&self, id: u32) {
        self.reuse.lock().unwrap().push(id)
    }

    pub fn poll_completions(&self) -> Option<(u32, C)> {
        self.inner
            .get_ref()
            .queue
            .get_completion(ReceiveFlags::NON_BLOCK)
            .ok()
    }

    pub fn wait_for_completion(&self) -> Option<(u32, C)> {
        self.inner
            .get_ref()
            .queue
            .get_completion(ReceiveFlags::empty())
            .ok()
    }

    fn handle_completion(&self, id: u32, item: C) {
        let mut calls = self.calls.lock().unwrap();
        let call = calls
            .remove(&id)
            .expect("failed to find registered callback");
        let mut call = call.lock().unwrap();
        call.item = Some((id, item));
        if let Some(waker) = call.waker.take() {
            waker.wake();
        }
    }

    /// Submit a request and don't wait for a response. WARNING: This will burn a request ID,
    /// preventing it from ever being reused. This function is mostly useful for signalling an "end
    /// of communication" event across the queue. If you want to submit and not immediately await,
    /// you probably should create a task for your async block instead.
    pub fn submit_no_wait(&self, item: S, flags: SubmissionFlags) {
        let _ = self
            .inner
            .get_ref()
            .queue
            .submit(self.next_id(), item, flags);
    }

    /// Submit an item and await a completion.
    pub async fn submit_and_wait(&self, item: S) -> Result<C, std::io::Error> {
        let id = self.next_id();
        let state = Arc::new(Mutex::new(WaitPoint::<C> {
            item: None,
            waker: None,
        }));
        {
            let mut calls = self.calls.lock().unwrap();
            calls.insert(id, state.clone());
            drop(calls);
        }
        if let Some((id, item)) = self.poll_completions() {
            self.handle_completion(id, item);
        }
        self.inner
            .write_with(|inner| {
                inner
                    .queue
                    .submit(id, item, SubmissionFlags::NON_BLOCK)
                    .map_err(|e| e.into())
            })
            .await?;

        let waiter = WaitPointFuture::<S, C> {
            state,
            sender: self,
        };
        let mut item = Box::pin(async { waiter.await }).fuse();
        let mut recv = Box::pin(async {
            loop {
                let (id, item) = self
                    .inner
                    .read_with(|inner| {
                        inner
                            .queue
                            .get_completion(ReceiveFlags::NON_BLOCK)
                            .map_err(|e| e.into())
                    })
                    .await
                    .unwrap();
                self.handle_completion(id, item);
            }
        })
        .fuse();
        let result = futures::select! {
            item_res = item => item_res,
            recv_res = recv => recv_res,
        }?;
        unsafe { self.release_id(id) };
        Ok(result.1)
    }
}
