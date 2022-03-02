use std::{task::{Waker, Poll}, sync::{Mutex, Arc, atomic::{AtomicU32, Ordering}}, future::Future, collections::BTreeMap};

use twizzler_async::{AsyncDuplex, AsyncDuplexSetup};
use twizzler_queue_raw::{QueueError, SubmissionFlags, ReceiveFlags};

use crate::Queue;

struct QueueSenderInner<S, C> {
    queue: Queue<S, C>,
}

struct WaitPoint<C> {
    item: Option<(u32, C)>,
    waker: Option<Waker>,
}

struct WaitPointFuture<'a, S, C> {
    state: Arc<Mutex<WaitPoint<C>>>,
    sender: &'a QueueSender<S, C>,
}

impl<'a, S: Copy, C: Copy> Future for WaitPointFuture<'a, S, C> {
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

pub struct QueueSender<S, C> {
    counter: AtomicU32,
    reuse: Mutex<Vec<u32>>,
    inner: AsyncDuplex<QueueSenderInner<S, C>>,
    calls: Mutex<BTreeMap<u32, Arc<Mutex<WaitPoint<C>>>>>,
}

impl<S: Copy, C: Copy> AsyncDuplexSetup for QueueSenderInner<S, C> {
    type ReadError = QueueError;
    type WriteError = QueueError;

    const READ_WOULD_BLOCK: Self::ReadError = QueueError::WouldBlock;
    const WRITE_WOULD_BLOCK: Self::WriteError = QueueError::WouldBlock;

    fn setup_read_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        self.queue.setup_read_com_sleep()
    }

    fn setup_write_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        self.queue.setup_write_sub_sleep()
    }
}

impl<S: Copy, C: Copy> QueueSender<S, C> {
    pub fn new(queue: Queue<S, C>) -> Self {
        Self {
            counter: AtomicU32::new(0),
            reuse: Mutex::new(vec![]),
            inner: AsyncDuplex::new(QueueSenderInner { queue }),
            calls: Mutex::new(BTreeMap::new()),
        }
    }

    fn next_id(&self) -> u32 {
        let mut reuse = self.reuse.lock().unwrap();
        reuse
            .pop()
            .unwrap_or_else(|| self.counter.fetch_add(1, Ordering::SeqCst))
    }

    fn release_id(&self, id: u32) {
        self.reuse.lock().unwrap().push(id)
    }

    fn poll_completions(&self) -> Option<(u32, C)> {
        Some(
            self.inner
                .get_ref()
                .queue
                .get_completion(ReceiveFlags::NON_BLOCK)
                .ok()?,
        )
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

    pub async fn submit_and_wait(&self, item: S) -> Result<C, crate::QueueError> {
        let id = self.next_id();
        let state = Arc::new(Mutex::new(WaitPoint::<C> {
            item: None,
            waker: None,
        }));
        let mut calls = self.calls.lock().unwrap();
        calls.insert(id, state.clone());
        drop(calls);
        if let Some((id, item)) = self.poll_completions() {
            self.handle_completion(id, item);
        }
        self.inner
            .write_with(|inner| inner.queue.submit(id, item, SubmissionFlags::NON_BLOCK))
            .await?;

        let waiter = WaitPointFuture::<S, C> {
            state,
            sender: self,
        };
        let item = Box::pin(waiter);
        let recv = Box::pin(async {
            loop {
                let (id, item) = self
                    .inner
                    .read_with(|inner| inner.queue.get_completion(ReceiveFlags::NON_BLOCK))
                    .await
                    .unwrap();
                self.handle_completion(id, item);
            }
        });
        let result = twizzler_async::wait_for_first(item, recv).await?;
        self.release_id(id);
        Ok(result.1)
    }
}
