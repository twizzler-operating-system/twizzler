mod async_ids;
mod inflight;
mod requester;
mod response_info;
mod submit;
mod summary;

#[async_trait::async_trait]
/// A trait implemented by a particular driver that can the be used by a [requester::Requester].
pub trait RequestDriver {
    /// The type of a request that will be used by the SubmitRequest wrapper to submit requests to
    /// the driver.
    type Request: Copy + Send;
    /// The type of a response to a request that we will use to send back to the Requester via the
    /// [requester::Requester::finish] function.
    type Response: Copy + Send;
    /// The type of a submit error in case submission fails.
    type SubmitError;
    /// The actual submit function. The driver should perform whatever device-specific submission
    /// procedure it needs to to submit all requests.
    async fn submit(&self, reqs: &[SubmitRequest<Self::Request>]) -> Result<(), Self::SubmitError>;
    /// Manually flush any internal driver submission queue.
    fn flush(&self);
    /// The number of IDs to have in-flight at a time.
    const NUM_IDS: usize;
}

// TODO: drop for inflight tracker, so we can remove it to save work?

pub use inflight::InFlightFuture;
pub use inflight::InFlightFutureWithResponses;
pub use requester::Requester;
pub use response_info::ResponseInfo;
pub use submit::SubmitRequest;
pub use summary::SubmitSummary;
pub use summary::SubmitSummaryWithResponses;
