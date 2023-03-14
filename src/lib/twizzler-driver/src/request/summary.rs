#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A summary of the result of submitting a collection of requests to the request manager and having
/// the device respond. Contains responses.
pub enum SubmitSummaryWithResponses<R> {
    /// A vector of responses in the same order as the submitted requests.
    Responses(Vec<R>),
    /// At least one error occurred. The usize value is the index of the first error.
    Errors(usize, Vec<R>),
    /// The request engine was shutdown while the requests were inflight.
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum AnySubmitSummary<R> {
    Done,
    Responses(Vec<R>),
    Errors(usize, Vec<R>),
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A summary of the result of submitting a collection of requests to the request manager and having
/// the device respond. Does not contain responses.
pub enum SubmitSummary {
    /// All requests completed successfully.
    Done,
    /// At least one error occurred. The usize value is the index of the first error.
    Errors(usize),
    /// The request engine was shutdown while the requests were inflight.
    Shutdown,
}

impl<R> From<AnySubmitSummary<R>> for SubmitSummary {
    fn from(a: AnySubmitSummary<R>) -> Self {
        match a {
            AnySubmitSummary::Done => SubmitSummary::Done,
            AnySubmitSummary::Responses(_) => panic!("cannot convert"),
            AnySubmitSummary::Errors(e, _) => SubmitSummary::Errors(e),
            AnySubmitSummary::Shutdown => SubmitSummary::Shutdown,
        }
    }
}

impl<R> From<AnySubmitSummary<R>> for SubmitSummaryWithResponses<R> {
    fn from(a: AnySubmitSummary<R>) -> Self {
        match a {
            AnySubmitSummary::Responses(r) => SubmitSummaryWithResponses::Responses(r),
            AnySubmitSummary::Done => panic!("cannot convert"),
            AnySubmitSummary::Errors(e, r) => SubmitSummaryWithResponses::Errors(e, r),
            AnySubmitSummary::Shutdown => SubmitSummaryWithResponses::Shutdown,
        }
    }
}
