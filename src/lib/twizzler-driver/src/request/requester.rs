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
pub struct Requester<T: RequestDriver> {
    driver: T,
    inflights: Mutex<HashMap<u64, Arc<InFlight<T::Response>>>>,
    ids: AsyncIdAllocator,
    state: AtomicU32,
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
            inflight.insert_to_map(reqs, idx_off);
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
        Ok(InFlightFuture::new(inflight))
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
        Ok(InFlightFutureWithResponses::new(inflight))
    }

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
