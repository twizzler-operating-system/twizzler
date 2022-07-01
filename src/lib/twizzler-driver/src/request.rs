use std::{
    collections::VecDeque,
    ops::Range,
    sync::Arc,
    task::{Poll, Waker},
};

use futures::lock::Mutex;

pub enum SubmitSummary {
    Done,
    Errors(usize),
}

pub struct SubmitRequest<T> {
    id: u64,
    data: T,
}

struct InFlightInner {
    waker: Option<Waker>,
    ready: Option<SubmitSummary>,
    count: usize,
    first_err: usize,
}

pub struct InFlight {
    ids: Range<u64>,
    inner: Arc<std::sync::Mutex<InFlightInner>>,
}

impl std::future::Future for InFlight {
    type Output = SubmitSummary;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(out) = inner.ready.take() {
            Poll::Ready(out)
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[async_trait::async_trait]
pub trait RequestDriver {
    type Request;
    type Response: Copy;
    type SubmitError;
    async fn submit(&self, reqs: &[SubmitRequest<Self::Request>]) -> Result<(), Self::SubmitError>;
    fn flush(&self);
}

struct PrepRequest<'a, T> {
    reqs: &'a [T],
}

struct RequesterInner<'a, T> {
    queue: VecDeque<PrepRequest<'a, T>>,
}

pub struct Requester<'a, T: RequestDriver> {
    driver: T,
    inner: Mutex<RequesterInner<'a, T::Request>>,
}

pub struct ResponseInfo<R> {
    resp: R,
    is_err: bool,
    id: u64,
}

impl<'a, T: RequestDriver> Requester<'a, T> {
    pub fn new(driver: T) -> Self {
        Self {
            driver,
            inner: Mutex::new(RequesterInner {
                queue: VecDeque::new(),
            }),
        }
    }

    async fn allocate_id(&self) -> u64 {
        todo!()
    }

    async fn release_id(&self, id: u64) {
        todo!()
    }

    async fn create_inflight(&self, len: usize) -> Result<InFlight, T::SubmitError> {
        todo!()
    }

    #[inline]
    pub async fn new_request(&self, data: T::Request) -> SubmitRequest<T::Request> {
        todo!()
    }

    pub async fn submit(
        &self,
        reqs: &'a [SubmitRequest<T::Request>],
        resps: &'a mut [T::Response],
    ) -> Result<InFlight, T::SubmitError> {
        let mut inner = self.inner.lock().await;
        let inflight = self.create_inflight(reqs.len()).await?;

        self.driver.submit(reqs).await?;
        Ok(inflight)
    }

    fn lookup_inflight(&self, id: u64) -> &InFlight {
        todo!()
    }

    pub fn finish(&self, resps: &[ResponseInfo<T::Response>]) {
        for resp in resps {
            let inflight = self.lookup_inflight(resp.id);
            let mut inner = inflight.inner.lock().unwrap();
            inner.count += 1;
            let idx = inflight.ids.as_index(resp.id);
            if resp.is_err && inner.first_err > idx {
                inner.first_err = idx;
            }

            if inner.count == inflight.ids.len() {
                inner.ready = Some(if inner.first_err == usize::MAX {
                    SubmitSummary::Done
                } else {
                    SubmitSummary::Errors(inner.first_err)
                });
                if let Some(w) = inner.waker.take() {
                    w.wake();
                }
            }
        }
    }
}
