//! A system for handling requests and organizing inflight requests while waiting for responses.
//!
//! The general structure of this system is that software implements the [RequestDriver] trait with
//! some struct that we'll call "the request driver" or just "the driver". The driver is then
//! wrapped by a [Requester], which internally manages the asynchrony of talking to devices.
//!
//! A user of the requester can call the [Requester::submit] or [Requester::submit_for_response]
//! functions to submit a set a requests depending on if the caller wants the responses or just
//! wants to know if the requests succeeded. The reason this distinction is maintained is that
//! collecting responses has an overhead. The requester interacts with the driver to submit the requests.
//!
//! Internally, the requester assigns IDs to requests for use in communicating with the driver.
//! These IDs are not necessarily allocated sequentially and can only be relied upon to be unique
//! while a given request is inflight.
//!
//! Once a request is completed by the driver, the driver should send the response data and ID of
//! the request that completed back to the requester with the [Requester::finish] function. The
//! request manager will then collate the responses for matching with the requests and any errors
//! are tracked. Once all requests in a submitted set have been completed, that set of requests is
//! finished and awaiting on it will return a [SubmitSummary] or a [SubmitSummaryWithResponses].

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
    async fn submit(
        &self,
        reqs: &mut [SubmitRequest<Self::Request>],
    ) -> Result<(), Self::SubmitError>;
    /// Manually flush any internal driver submission queue.
    fn flush(&self);
    /// The number of IDs to have in-flight at a time.
    fn num_ids(&self) -> usize;
}

// TODO: drop for inflight tracker, so we can remove it to save work?

pub use inflight::InFlightFuture;
pub use inflight::InFlightFutureWithResponses;
pub use requester::Requester;
pub use response_info::ResponseInfo;
pub use submit::SubmitError;
pub use submit::SubmitRequest;
pub use summary::SubmitSummary;
pub use summary::SubmitSummaryWithResponses;
