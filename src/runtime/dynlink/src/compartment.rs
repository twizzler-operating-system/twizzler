//! Compartments are an abstraction for isolation of library components, but they are not done yet.

use std::{
    collections::HashMap,
    fmt::{Debug, Display},
};

use petgraph::stable_graph::NodeIndex;
use talc::{ErrOnOom, Talc};

use crate::{engines::Backing, library::LibraryId, tls::TlsInfo};

mod tls;

#[repr(C)]
/// A compartment that contains libraries (and a local runtime).
pub struct Compartment {
    pub name: String,
    pub id: CompartmentId,
    // Library names are per-compartment.
    pub(crate) library_names: HashMap<String, NodeIndex>,
    // We maintain an allocator, so we can alloc data within the compartment.
    pub(super) allocator: Talc<ErrOnOom>,
    pub(super) alloc_objects: Vec<Backing>,

    // Information for TLS. We store all the "active" generations.
    pub(crate) tls_info: HashMap<u64, TlsInfo>,
    pub(crate) tls_gen: u64,
}

/// ID type for a compartment.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(transparent)]
pub struct CompartmentId(pub(crate) usize);

impl Display for CompartmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CompartmentId {
    /// Get the raw integer representing compartment ID.
    pub fn raw(&self) -> usize {
        self.0
    }
}

pub const MONITOR_COMPARTMENT_ID: CompartmentId = CompartmentId(0);
impl Compartment {
    pub(crate) fn new(name: String, id: CompartmentId) -> Self {
        Self {
            name,
            id,
            library_names: HashMap::new(),
            allocator: Talc::new(ErrOnOom),
            alloc_objects: vec![],
            tls_info: HashMap::new(),
            tls_gen: 0,
        }
    }

    /// Get an iterator over the IDs of libraries in this compartment.
    pub fn library_ids(&self) -> impl Iterator<Item = LibraryId> + '_ {
        self.library_names.values().map(|idx| LibraryId(*idx))
    }
}

impl core::fmt::Display for Compartment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Debug for Compartment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compartment[{}]", self.name)
    }
}
