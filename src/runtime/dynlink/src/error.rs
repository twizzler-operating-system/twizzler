//! Definitions for errors for the dynamic linker.
use std::sync::PoisonError;

use miette::Diagnostic;
use thiserror::Error;

use crate::library::{BackingData, Library};

impl<T> From<PoisonError<T>> for DynlinkError {
    fn from(value: PoisonError<T>) -> Self {
        Self {
            kind: Self::PoisonError {
                report: value.to_string(),
            },
            related: vec![],
            library: "".to_string(),
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
#[error("dynamic linker error")]
pub struct DynlinkError {
    pub kind: DynlinkErrorKind,
    #[related]
    pub related: Vec<DynlinkError>,
    pub library: String,
}

impl<B: BackingData> Library<B> {
    pub(crate) fn errors(
        &self,
        kind: DynlinkErrorKind,
        related: Vec<DynlinkError>,
    ) -> DynlinkError {
        DynlinkError {
            kind,
            related,
            library: self.name.clone(),
        }
    }

    pub(crate) fn error(&self, kind: DynlinkErrorKind) -> DynlinkError {
        DynlinkError {
            kind,
            related,
            library: self.name.clone(),
        }
    }
}

impl DynlinkError {
    pub fn new(kind: DynlinkErrorKind, related: Vec<DynlinkError>, library: String) -> Self {
        Self {
            kind,
            related,
            library,
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum DynlinkErrorKind {
    #[error("failed to load library")]
    LibraryLoadFail,
    #[error("name not found: {name}")]
    NameNotFound { name: String },
    #[error("name already exists: {name}")]
    NameAlreadyExists { name: String },
    #[error("parse failed: {err}")]
    ParseError {
        #[from]
        err: elf::ParseError,
    },
    #[error(transparent)]
    PoisonError { report: String },
}
