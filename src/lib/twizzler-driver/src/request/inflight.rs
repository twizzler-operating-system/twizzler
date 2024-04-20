use std::{
    collections::HashMap,
    mem::MaybeUninit,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

use super::{
    response_info::ResponseInfo,
    submit::SubmitRequest,
    summary::{AnySubmitSummary, SubmitSummary, SubmitSummaryWithResponses},
};

#[derive(Debug)]
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

    fn finish(&mut self, val: AnySubmitSummary<R>) {
        if self.ready.is_some() {
            return;
        }
        self.ready = Some(val);
        if let Some(w) = self.waker.take() {
            w.wake();
        }
    }

    fn count(&self) -> usize {
        self.count
    }

    fn calc_summary(&mut self) -> AnySubmitSummary<R> {
        if self.first_err == usize::MAX {
            if let Some(resps) = self.resps.take() {
                let arr = resps.into_raw_parts();
                let na = unsafe { Vec::from_raw_parts(arr.0 as *mut R, arr.1, arr.2) };
                AnySubmitSummary::Responses(na)
            } else {
                AnySubmitSummary::Done
            }
        } else {
            if let Some(resps) = self.resps.take() {
                let arr = resps.into_raw_parts();
                let na = unsafe { Vec::from_raw_parts(arr.0 as *mut R, arr.1, arr.2) };
                AnySubmitSummary::Errors(self.first_err, na)
            } else {
                AnySubmitSummary::Errors(self.first_err, vec![])
            }
        }
    }

    fn tally_resp(&mut self, resp: &ResponseInfo<R>)
    where
        R: Send + Copy,
    {
        self.count += 1;

        if self.resps.is_some() {
            let idx = *self
                .map
                .get(&resp.id())
                .expect("failed to lookup ID in ID map");
            if resp.is_err() && self.first_err > idx {
                self.first_err = idx;
            }
            self.resps.as_mut().unwrap()[idx] = MaybeUninit::new(*resp.data());
        } else {
            if resp.is_err() {
                self.first_err = 0;
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct InFlight<R> {
    len: usize,
    inner: Arc<Mutex<InFlightInner<R>>>,
}

impl<R> InFlight<R> {
    pub(crate) fn new(len: usize, resps: bool) -> Self {
        Self {
            len,
            inner: Arc::new(Mutex::new(InFlightInner::new(resps, len))),
        }
    }

    pub(crate) fn finish(&self, summ: AnySubmitSummary<R>) {
        let mut inner = self.inner.lock().unwrap();
        inner.finish(summ);
    }

    pub(crate) fn insert_to_map<T>(&self, reqs: &[SubmitRequest<T>], idx_off: usize) {
        let mut inner = self.inner.lock().unwrap();
        if inner.resps.is_some() {
            for (idx, req) in reqs.iter().enumerate() {
                inner.map.insert(req.id(), idx_off + idx);
            }
        }
    }

    pub(crate) fn handle_resp(&self, resp: &ResponseInfo<R>)
    where
        R: Send + Copy,
    {
        let mut inner = self.inner.lock().unwrap();
        inner.tally_resp(resp);
        if inner.count() == self.len {
            let summ = inner.calc_summary();
            inner.finish(summ);
        }
    }
}

#[derive(Debug)]
/// A future for a set of in-flight requests for which we are uninterested in any responses from the
/// device, we only care if the responses were completed successfully or not. On await, returns a
/// [SubmitSummary].
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

impl<R> InFlightFuture<R> {
    pub(crate) fn new(inflight: Arc<InFlight<R>>) -> Self {
        Self { inflight }
    }
}

impl<R> InFlightFutureWithResponses<R> {
    pub(crate) fn new(inflight: Arc<InFlight<R>>) -> Self {
        Self { inflight }
    }
}

#[derive(Debug)]
/// A future for a set of in-flight requests for which we are interested in all responses from the
/// device. On await, returns a [SubmitSummaryWithResponses].
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
