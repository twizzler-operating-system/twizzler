mod batch;
mod object;
mod unsafetx;

pub use batch::*;
pub use object::*;
pub use unsafetx::*;

/// A trait for implementing per-object transaction handles.
pub trait TxHandle {
    /// Ensures transactional safety for mutably accessing data in the range [data, data + len).
    fn tx_mut(&self, data: *const u8, len: usize) -> Result<*mut u8>;
}

pub type Result<T> = std::result::Result<T, TxError>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, thiserror::Error)]
/// Transaction errors, with user-definable abort type.
pub enum TxError {
    /// Resources exhausted.
    #[error("resources exhausted")]
    Exhausted,
    /// Tried to mutate immutable data.
    #[error("object is immutable")]
    Immutable,
    /// Invalid argument.
    #[error("invalid argument")]
    InvalidArgument,
}
