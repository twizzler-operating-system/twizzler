/// A trait for implementing transaction handles.
///
/// Takes a lifetime argument, 'obj. All object handles referenced by this transaction must have
/// this lifetime or longer.
pub trait TxHandle<'obj> {
    /// Ensures transactional safety for mutably accessing data given by the range [data, data +
    /// sizeof(T)).
    fn tx_mut<T, E>(&self, data: *const T) -> TxResult<*mut T, E>;
}

pub type TxResult<T, E = ()> = Result<T, TxError<E>>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, thiserror::Error)]
/// Transaction errors, with user-definable abort type.
pub enum TxError<E> {
    /// Transaction aborted.
    #[error("aborted: {0}")]
    Abort(E),
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
