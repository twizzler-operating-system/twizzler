//! Definitions for errors for the dynamic linker.
use std::alloc::Layout;

use elf::file::Class;
use itertools::{Either, Itertools};
use miette::Diagnostic;
use thiserror::Error;

use crate::{
    compartment::CompartmentId,
    engines::LoadDirective,
    library::{LibraryId, UnloadedLibrary},
};

#[derive(Debug, Error, Diagnostic)]
#[error("{kind}")]
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
        // Collect errors and values, and then if there any errors, build a new error from them.
        let (vals, errs): (Vec<T>, Vec<DynlinkError>) =
            it.into_iter().partition_map(|item| match item {
                Ok(o) => Either::Left(o),
                Err(e) => Either::Right(e),
            });

        if errs.is_empty() {
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
    #[error("failed to find symbol '{symname}' for '{sourcelib}'")]
    SymbolLookupFail { symname: String, sourcelib: String },
    #[error("name already exists: {name}")]
    NameAlreadyExists { name: String },
    #[error("parse failed: {err}")]
    ParseError {
        #[from]
        err: elf::ParseError,
    },
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
    UnsupportedReloc { library: String, reloc: String },
    #[error("failed to process relocation section '{secname}' for library '{library}'")]
    RelocationSectionFail { secname: String, library: String },
    #[error("library '{library}' failed to relocate")]
    RelocationFail { library: String },
    #[error("failed to create new backing data")]
    NewBackingFail,
    #[error("failed to satisfy load directive")]
    LoadDirectiveFail { dir: LoadDirective },
    #[error("tried to operate on an unloaded library '{library}'")]
    UnloadedLibrary { library: String },
    #[error("dependencies of '{library}' failed to relocate")]
    DepsRelocFail { library: String },
    #[error("invalid library ID '{id}'")]
    InvalidLibraryId { id: LibraryId },
    #[error("invalid compartment ID '{id}'")]
    InvalidCompartmentId { id: CompartmentId },
    #[error("invalid ELF header: {hdr_err}")]
    InvalidELFHeader {
        #[source]
        #[from]
        #[diagnostic_source]
        hdr_err: HeaderError,
    },
    #[error("no entry address present")]
    NoEntryAddress { name: String },
}

#[derive(Debug, Error, Diagnostic)]
pub enum HeaderError {
    #[error("class mismatch: expected {expect:?}, got {got:?}")]
    ClassMismatch { expect: Class, got: Class },
    #[error("ELF version mismatch: expected {expect}, got {got}")]
    VersionMismatch { expect: u32, got: u32 },
    #[error("OS/ABI mismatch: expected {expect}, got {got}")]
    OSABIMismatch { expect: u8, got: u8 },
    #[error("ABI version mismatch: expected {expect}, got {got}")]
    ABIVersionMismatch { expect: u8, got: u8 },
    #[error("ELF type mismatch: expected {expect}, got {got}")]
    ELFTypeMismatch { expect: u16, got: u16 },
    #[error("machine mismatch: expected {expect}, got {got}")]
    MachineMismatch { expect: u16, got: u16 },
}

impl From<elf::ParseError> for DynlinkError {
    fn from(value: elf::ParseError) -> Self {
        Self {
            kind: DynlinkErrorKind::ParseError { err: value },
            related: vec![],
        }
    }
}
