use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
};

use super::{
    async_ids::AsyncIdAllocator,
    inflight::{InFlight, InFlightFuture, InFlightFutureWithResponses},
    response_info::ResponseInfo,
    submit::{SubmitError, SubmitRequest},
    summary::AnySubmitSummary,
    RequestDriver,
};

const OK: u32 = 0;
const SHUTDOWN: u32 = 1;

/// A wrapper for managing requests and responses for a given driver.
pub struct Requester<T: RequestDriver> {
    driver: T,
    inflights: Mutex<HashMap<u64, Arc<InFlight<T::Response>>>>,
    ids: AsyncIdAllocator,
    state: AtomicU32,
}

impl<T: RequestDriver> Requester<T> {
    /// Get a reference to the driver.
    pub fn driver(&self) -> &T {
        &self.driver
    }

    /// Check if the requester is shutdown.
    pub fn is_shutdown(&self) -> bool {
        self.state.load(Ordering::SeqCst) == SHUTDOWN
    }

    /// Construct a new request manager for a given driver.
    pub fn new(driver: T) -> Self {
        Self {
            ids: AsyncIdAllocator::new(driver.num_ids()),
            driver,
            inflights: Mutex::new(HashMap::new()),
            state: AtomicU32::new(OK),
        }
    }

    async fn allocate_ids(&self, reqs: &mut [SubmitRequest<T::Request>]) -> usize {
        for (num, req) in reqs.iter_mut().enumerate() {
            if num == 0 {
                req.set_id(self.ids.next().await);
            } else {
                if let Some(id) = self.ids.try_next() {
                    req.set_id(id);
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
                if map.insert(req.id(), inflight.clone()).is_some() {
                    panic!("tried to map existing in-flight request");
                }
            }
        }
        inflight.insert_to_map(reqs, idx_off);
    }

    async fn do_submit(
        &self,
        inflight: Arc<InFlight<T::Response>>,
        reqs: &mut [SubmitRequest<T::Request>],
    ) -> Result<(), SubmitError<T::SubmitError>> {
        let mut idx = 0;
        while idx < reqs.len() {
            let count = self.allocate_ids(&mut reqs[idx..]).await;
            self.map_inflight(inflight.clone(), &reqs[idx..(idx + count)], idx);
            self.driver
                .submit(&mut reqs[idx..(idx + count)])
                .await
                .map_err(|e| SubmitError::DriverError(e))?;
            idx += count;
        }
        Ok(())
    }

    /// Submit a set of requests, for which we are **not** interested in the specific responses from the
    /// device. Returns a future that awaits on an [InFlightFuture], so awaiting on this function
    /// ensures that all requests are submitted, not necessarily handled.
    pub async fn submit(
        &self,
        reqs: &mut [SubmitRequest<T::Request>],
    ) -> Result<InFlightFuture<T::Response>, SubmitError<T::SubmitError>> {
        if self.is_shutdown() {
            return Err(SubmitError::IsShutdown);
        }
        let inflight = Arc::new(InFlight::new(reqs.len(), false));

        self.do_submit(inflight.clone(), reqs).await?;
        Ok(InFlightFuture::new(inflight))
    }

    /// Submit a set of requests, for which we **are** interested in the specific responses from the
    /// device. Returns a future that awaits on an [InFlightFutureWithResponses], so awaiting on this function
    /// ensures that all requests are submitted, not necessarily handled.
    pub async fn submit_for_response(
        &self,
        reqs: &mut [SubmitRequest<T::Request>],
    ) -> Result<InFlightFutureWithResponses<T::Response>, SubmitError<T::SubmitError>> {
        if self.is_shutdown() {
            return Err(SubmitError::IsShutdown);
        }
        let inflight = Arc::new(InFlight::new(reqs.len(), true));
        self.do_submit(inflight.clone(), reqs).await?;
        Ok(InFlightFutureWithResponses::new(inflight))
    }

    /// Shutdown the request manager.
    pub fn shutdown(&self) {
        self.state.store(SHUTDOWN, Ordering::SeqCst);
        let mut inflights = self.inflights.lock().unwrap();
        for (_, inflight) in inflights.drain() {
            inflight.finish(AnySubmitSummary::Shutdown);
        }
    }

    fn take_inflight(&self, id: u64) -> Option<Arc<InFlight<T::Response>>> {
        self.inflights.lock().unwrap().remove(&id)
    }

    /// Send back, from the driver, to the request manager, a set of responses to a previously
    /// submitted set of requests. The responses need not be contiguous in ID, nor do they need all
    /// be from the same set of requests.
    pub fn finish(&self, resps: &[ResponseInfo<T::Response>]) {
        if self.is_shutdown() {
            return;
        }
        for resp in resps {
            let inflight = self.take_inflight(resp.id());
            if let Some(inflight) = inflight {
                inflight.handle_resp(resp);
            }

            self.release_id(resp.id());
        }
    }
}
