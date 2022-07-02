use std::{
    collections::HashMap,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    task::{Poll, Waker},
};

use self::async_ids::AsyncIdAllocator;

mod async_ids;

#[derive(Clone, Debug)]
pub enum SubmitSummaryWithResponses<R> {
    Responses(Vec<R>),
    Errors(usize),
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum AnySubmitSummary<R> {
    Done,
    Responses(Vec<R>),
    Errors(usize),
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
pub enum SubmitSummary {
    Done,
    Errors(usize),
    Shutdown,
}

impl<R> From<AnySubmitSummary<R>> for SubmitSummary {
    fn from(a: AnySubmitSummary<R>) -> Self {
        match a {
            AnySubmitSummary::Done => SubmitSummary::Done,
            AnySubmitSummary::Responses(_) => panic!("cannot convert"),
            AnySubmitSummary::Errors(e) => SubmitSummary::Errors(e),
            AnySubmitSummary::Shutdown => SubmitSummary::Shutdown,
        }
    }
}

impl<R> From<AnySubmitSummary<R>> for SubmitSummaryWithResponses<R> {
    fn from(a: AnySubmitSummary<R>) -> Self {
        match a {
            AnySubmitSummary::Responses(r) => SubmitSummaryWithResponses::Responses(r),
            AnySubmitSummary::Done => panic!("cannot convert"),
            AnySubmitSummary::Errors(e) => SubmitSummaryWithResponses::Errors(e),
            AnySubmitSummary::Shutdown => SubmitSummaryWithResponses::Shutdown,
        }
    }
}

#[derive(Debug)]
pub struct SubmitRequest<T> {
    id: u64,
    data: T,
}

impl<T> SubmitRequest<T> {
    pub fn new(data: T) -> Self {
        Self { id: 0, data }
    }

    pub fn data(&self) -> &T {
        &self.data
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

struct InFlightInner<R> {
    waker: Option<Waker>,
    ready: Option<AnySubmitSummary<R>>,
    count: usize,
    first_err: usize,
    resps: Option<Vec<MaybeUninit<R>>>,
    map: HashMap<u64, usize>,
}

impl<R> InFlightInner<R> {
    fn new(resps: bool, len: usize) -> Self {
        let mut s = Self {
            waker: None,
            ready: None,
            count: 0,
            first_err: usize::MAX,
            resps: if resps {
                Some(Vec::with_capacity(len))
            } else {
                None
            },
            map: HashMap::new(),
        };
        if let Some(v) = s.resps.as_mut() {
            v.resize_with(len, || MaybeUninit::uninit());
        }
        s
    }
}

pub struct InFlight<R> {
    len: usize,
    inner: Arc<Mutex<InFlightInner<R>>>,
}

impl<R> InFlight<R> {
    fn new(len: usize, resps: bool) -> Self {
        Self {
            len,
            inner: Arc::new(Mutex::new(InFlightInner::new(resps, len))),
        }
    }
}

pub struct InFlightFuture<R> {
    inflight: Arc<InFlight<R>>,
}

impl<R> std::future::Future for InFlightFuture<R> {
    type Output = SubmitSummary;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut inner = self.inflight.inner.lock().unwrap();
        if let Some(out) = inner.ready.take() {
            Poll::Ready(out.into())
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

pub struct InFlightFutureWithResponses<R> {
    inflight: Arc<InFlight<R>>,
}

impl<R> std::future::Future for InFlightFutureWithResponses<R> {
    type Output = SubmitSummaryWithResponses<R>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut inner = self.inflight.inner.lock().unwrap();
        if let Some(out) = inner.ready.take() {
            Poll::Ready(out.into())
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[async_trait::async_trait]
pub trait RequestDriver {
    type Request;
    type Response: Copy + Send;
    type SubmitError;
    async fn submit(&self, reqs: &[SubmitRequest<Self::Request>]) -> Result<(), Self::SubmitError>;
    fn flush(&self);
    const NUM_IDS: usize;
}

const OK: u32 = 0;
const SHUTDOWN: u32 = 1;
pub struct Requester<T: RequestDriver> {
    driver: T,
    inflights: Mutex<HashMap<u64, Arc<InFlight<T::Response>>>>,
    ids: AsyncIdAllocator,
    state: AtomicU32,
}

pub struct ResponseInfo<R: Send> {
    resp: R,
    is_err: bool,
    id: u64,
}

impl<R: Send> ResponseInfo<R> {
    pub fn new(resp: R, id: u64, is_err: bool) -> Self {
        Self { resp, is_err, id }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubmitError<E> {
    DriverError(E),
    IsShutdown,
}

impl<T: RequestDriver> Requester<T> {
    pub fn is_shutdown(&self) -> bool {
        self.state.load(Ordering::SeqCst) == SHUTDOWN
    }

    pub fn new(driver: T) -> Self {
        Self {
            ids: AsyncIdAllocator::new(T::NUM_IDS),
            driver,
            inflights: Mutex::new(HashMap::new()),
            state: AtomicU32::new(OK),
        }
    }

    async fn allocate_ids(&self, reqs: &mut [SubmitRequest<T::Request>]) -> usize {
        for (num, req) in reqs.iter_mut().enumerate() {
            if num == 0 {
                req.id = self.ids.next().await;
            } else {
                if let Some(id) = self.ids.try_next() {
                    req.id = id;
                } else {
                    return num;
                }
            }
        }
        reqs.len()
    }

    fn release_id(&self, id: u64) {
        self.ids.release_id(id);
    }

    fn map_inflight(
        &self,
        inflight: Arc<InFlight<T::Response>>,
        reqs: &[SubmitRequest<T::Request>],
        idx_off: usize,
    ) {
        {
            let mut map = self.inflights.lock().unwrap();
            for req in reqs {
                if map.insert(req.id, inflight.clone()).is_some() {
                    panic!("tried to map existing in-flight request");
                }
            }
        }
        let mut inner = inflight.inner.lock().unwrap();
        if inner.resps.is_some() {
            for (idx, req) in reqs.iter().enumerate() {
                inner.map.insert(req.id, idx_off + idx);
            }
        }
    }

    pub async fn submit(
        &self,
        reqs: &mut [SubmitRequest<T::Request>],
    ) -> Result<InFlightFuture<T::Response>, SubmitError<T::SubmitError>> {
        if self.is_shutdown() {
            return Err(SubmitError::IsShutdown);
        }
        let inflight = Arc::new(InFlight::new(reqs.len(), false));

        let mut idx = 0;
        while idx < reqs.len() {
            let count = self.allocate_ids(&mut reqs[idx..]).await;
            self.map_inflight(inflight.clone(), &reqs[idx..(idx + count)], idx);
            self.driver
                .submit(&reqs[idx..(idx + count)])
                .await
                .map_err(|e| SubmitError::DriverError(e))?;
            idx += count;
        }
        Ok(InFlightFuture { inflight })
    }

    pub async fn submit_for_response(
        &self,
        reqs: &mut [SubmitRequest<T::Request>],
    ) -> Result<InFlightFutureWithResponses<T::Response>, SubmitError<T::SubmitError>> {
        if self.is_shutdown() {
            return Err(SubmitError::IsShutdown);
        }
        let inflight = Arc::new(InFlight::new(reqs.len(), true));

        let mut idx = 0;
        while idx < reqs.len() {
            let count = self.allocate_ids(&mut reqs[idx..]).await;
            self.map_inflight(inflight.clone(), &reqs[idx..(idx + count)], idx);
            self.driver
                .submit(&reqs[idx..(idx + count)])
                .await
                .map_err(|e| SubmitError::DriverError(e))?;
            self.driver.flush();
            idx += count;
        }
        Ok(InFlightFutureWithResponses { inflight })
    }

    pub fn shutdown(&self) {
        self.state.store(SHUTDOWN, Ordering::SeqCst);
        let mut inflights = self.inflights.lock().unwrap();
        for (_, inflight) in inflights.drain() {
            let mut inner = inflight.inner.lock().unwrap();
            if inner.ready.is_none() {
                inner.ready = Some(AnySubmitSummary::Shutdown);
                if let Some(w) = inner.waker.take() {
                    w.wake();
                }
            }
        }
    }

    fn take_inflight(&self, id: u64) -> Option<Arc<InFlight<T::Response>>> {
        self.inflights.lock().unwrap().remove(&id)
    }

    pub fn finish(&self, resps: &[ResponseInfo<T::Response>]) {
        if self.is_shutdown() {
            return;
        }
        for resp in resps {
            let inflight = self.take_inflight(resp.id);
            if let Some(inflight) = inflight {
                let mut inner = inflight.inner.lock().unwrap();
                inner.count += 1;

                if inner.resps.is_some() {
                    let idx = *inner
                        .map
                        .get(&resp.id)
                        .expect("failed to lookup ID in ID map");
                    inner.resps.as_mut().unwrap()[idx] = MaybeUninit::new(resp.resp);
                    if resp.is_err && inner.first_err > idx {
                        inner.first_err = idx;
                    }
                } else {
                    if resp.is_err {
                        inner.first_err = 0;
                    }
                }
                if inner.count == inflight.len {
                    inner.ready = Some(if inner.first_err == usize::MAX {
                        if let Some(resps) = inner.resps.take() {
                            let arr = resps.into_raw_parts();
                            let na = unsafe {
                                Vec::from_raw_parts(arr.0 as *mut T::Response, arr.1, arr.2)
                            };
                            AnySubmitSummary::Responses(na)
                        } else {
                            AnySubmitSummary::Done
                        }
                    } else {
                        AnySubmitSummary::Errors(inner.first_err)
                    });
                    if let Some(w) = inner.waker.take() {
                        w.wake();
                    }
                }
            }

            self.release_id(resp.id);
        }
    }
}

// TODO: drop for inflight tracker, so we can remove it to save work?
