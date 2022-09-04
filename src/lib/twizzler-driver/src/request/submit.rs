#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Error that can arise from submitting a set of requests.
pub enum SubmitError<E> {
    /// Error from the driver.
    DriverError(E),
    /// The request engine is shutdown.
    IsShutdown,
}
// TODO: impl Error

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A wrapper around a request that adds an ID alongside a request. The ID is automatically
/// allocated internally by the request manager after the [SubmitRequest] is submitted.
pub struct SubmitRequest<T> {
    id: u64,
    data: T,
}

impl<T> SubmitRequest<T> {
    /// Construct a new [SubmitRequest].
    pub fn new(data: T) -> Self {
        Self { id: 0, data }
    }

    /// Get a reference to the data.
    pub fn data(&self) -> &T {
        &self.data
    }

    /// Get a mutable reference to the data.
    pub fn data_mut(&mut self) -> &mut T {
        &mut self.data
    }
    /// Get the ID of the request. Note that this number is only meaningful after the request has
    /// been submitted.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn set_id(&mut self, id: u64) {
        self.id = id;
    }
}
