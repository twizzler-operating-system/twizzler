mod async_ids;
mod inflight;
mod requester;
mod response_info;
mod submit;
mod summary;

#[async_trait::async_trait]
pub trait RequestDriver {
    type Request: Copy + Send;
    type Response: Copy + Send;
    type SubmitError;
    async fn submit(&self, reqs: &[SubmitRequest<Self::Request>]) -> Result<(), Self::SubmitError>;
    fn flush(&self);
    const NUM_IDS: usize;
}

// TODO: drop for inflight tracker, so we can remove it to save work?

pub use requester::Requester;
pub use response_info::ResponseInfo;
pub use submit::SubmitRequest;
