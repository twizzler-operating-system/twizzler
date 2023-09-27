use crate::library::LibraryId;
use elf::ParseError;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum LookupError {
    #[error("not found")]
    NotFound,
    #[error("tried to use an unloaded library")]
    Unloaded,
    #[error("failed to parse object data: {0:?}")]
    ParseError(elf::ParseError),
}

#[derive(Debug, Error)]
pub enum AddLibraryError {
    #[error("library not found")]
    NotFound,
    #[error("failed to load library: {0}")]
    AdvanceError(AdvanceError),
}

#[derive(Debug, Error)]
pub enum AdvanceError {
    #[error("library error: {0:?}")]
    LibraryFailed(LibraryId),
    #[error("parsing failed")]
    ParseError(ParseError),
    #[error("library already initialized")]
    EndState,
}

impl From<ParseError> for AdvanceError {
    fn from(value: ParseError) -> Self {
        AdvanceError::ParseError(value)
    }
}

impl From<ParseError> for LookupError {
    fn from(value: ParseError) -> Self {
        LookupError::ParseError(value)
    }
}

impl From<AdvanceError> for AddLibraryError {
    fn from(value: AdvanceError) -> Self {
        Self::AdvanceError(value)
    }
}
