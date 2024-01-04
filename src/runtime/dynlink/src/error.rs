//! Definitions for errors for the dynamic linker.
use std::{alloc::Layout, sync::PoisonError};

use itertools::{Either, Itertools};
use miette::Diagnostic;
use thiserror::Error;

use crate::library::UnloadedLibrary;

impl<T> From<PoisonError<T>> for DynlinkError {
    fn from(value: PoisonError<T>) -> Self {
        Self {
            kind: DynlinkErrorKind::PoisonError {
                report: value.to_string(),
            },
            related: vec![],
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
#[error("dynamic linker error")]
pub struct DynlinkError {
    pub kind: DynlinkErrorKind,
    #[related]
    pub related: Vec<DynlinkError>,
}

impl DynlinkError {
    pub fn new_collect(kind: DynlinkErrorKind, related: Vec<DynlinkError>) -> Self {
        Self { kind, related }
    }

    pub fn new(kind: DynlinkErrorKind) -> Self {
        Self {
            kind,
            related: vec![],
        }
    }

    pub fn collect<I, T>(parent_kind: DynlinkErrorKind, it: I) -> Result<Vec<T>, DynlinkError>
    where
        I: IntoIterator<Item = Result<T, DynlinkError>>,
    {
        let (vals, errs): (Vec<T>, Vec<DynlinkError>) =
            it.into_iter().partition_map(|item| match item {
                Ok(o) => Either::Left(o),
                Err(e) => Either::Right(e),
            });

        if errs.len() == 0 {
            Ok(vals)
        } else {
            Err(DynlinkError {
                kind: parent_kind,
                related: errs,
            })
        }
    }
}

impl From<DynlinkErrorKind> for DynlinkError {
    fn from(value: DynlinkErrorKind) -> Self {
        Self {
            kind: value,
            related: vec![],
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum DynlinkErrorKind {
    #[error("failed to load library {library}")]
    LibraryLoadFail { library: UnloadedLibrary },
    #[error("name not found: {name}")]
    NameNotFound { name: String },
    #[error("name already exists: {name}")]
    NameAlreadyExists { name: String },
    #[error("parse failed: {err}")]
    ParseError {
        #[from]
        err: elf::ParseError,
    },
    #[error("poison error: {report}")]
    PoisonError { report: String },
    #[error("dynamic object is missing a required segment or section '{name}'")]
    MissingSection { name: String },
    #[error("failed to allocate {:?} within compartment {}", layout, comp)]
    FailedToAllocate { comp: String, layout: Layout },
    #[error("invalid allocation layout: {err}")]
    LayoutError {
        #[from]
        err: std::alloc::LayoutError,
    },
    #[error("failed to enumerate dependencies for {library}")]
    DepEnumerationFail { library: String },
    #[error("library {library} had no TLS data for request")]
    NoTLSInfo { library: String },
    #[error("library {library} requested relocation that is unsupported")]
    UnsupportedReloc { library: String, reloc: u32 },
    #[error("library {library} failed to relocate")]
    RelocationFail { library: String },
    #[error("failed to create new backing data")]
    NewBackingFail,
}

impl From<elf::ParseError> for DynlinkError {
    fn from(value: elf::ParseError) -> Self {
        Self {
            kind: DynlinkErrorKind::ParseError { err: value },
            related: vec![],
        }
    }
}
