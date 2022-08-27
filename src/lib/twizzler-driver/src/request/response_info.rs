#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Information about a response from the driver. Sent by the driver back to the request manager.
pub struct ResponseInfo<R> {
    resp: R,
    is_err: bool,
    id: u64,
}

impl<R: Send> ResponseInfo<R> {
    /// Construct a new ResponseInfo.
    pub fn new(resp: R, id: u64, is_err: bool) -> Self {
        Self { resp, is_err, id }
    }

    /// Is this response an error?
    pub fn is_err(&self) -> bool {
        self.is_err
    }

    /// Get a reference to the response data.
    pub fn data(&self) -> &R {
        &self.resp
    }

    /// Convert this ResponseInfo into the inner response data.
    pub fn into_inner(self) -> R {
        self.resp
    }

    /// Get the ID of the response (and thus the request with which it is paired).
    pub fn id(&self) -> u64 {
        self.id
    }
}
