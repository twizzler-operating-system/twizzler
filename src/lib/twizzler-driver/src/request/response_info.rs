#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResponseInfo<R> {
    resp: R,
    is_err: bool,
    id: u64,
}

impl<R: Send> ResponseInfo<R> {
    pub fn new(resp: R, id: u64, is_err: bool) -> Self {
        Self { resp, is_err, id }
    }

    pub fn is_err(&self) -> bool {
        self.is_err
    }

    pub fn data(&self) -> &R {
        &self.resp
    }

    pub fn into_inner(self) -> R {
        self.resp
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}
