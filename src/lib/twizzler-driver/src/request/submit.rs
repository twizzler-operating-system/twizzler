#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubmitError<E> {
    DriverError(E),
    IsShutdown,
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

    pub(crate) fn set_id(&mut self, id: u64) {
        self.id = id;
    }
}
