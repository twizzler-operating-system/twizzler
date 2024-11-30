use std::sync::PoisonError;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("unknown")]
    Unknown,
    #[error("lock poison error")]
    PoisonError,
}

impl<T> From<PoisonError<T>> for RuntimeError {
    fn from(_value: PoisonError<T>) -> Self {
        // TODO
        Self::PoisonError
    }
}
