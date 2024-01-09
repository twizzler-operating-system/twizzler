//! Compartments are an abstraction for isolation of library components, but they are not done yet.

use petgraph::stable_graph::NodeIndex;
use std::{
    collections::HashMap,
    fmt::{Debug, Display},
};

use talc::{ErrOnOom, Talc};

use crate::{library::BackingData, tls::TlsInfo};

mod alloc;
mod tls;

/// A compartment that contains libraries (and a local runtime).
pub struct Compartment<Backing: BackingData> {
    pub name: String,
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

impl<Backing: BackingData> Compartment<Backing> {
    pub(crate) fn new(name: String) -> Self {
        Self {
            name,
            library_names: HashMap::new(),
            allocator: Talc::new(ErrOnOom),
            alloc_objects: vec![],
            tls_info: HashMap::new(),
            tls_gen: 0,
        }
    }
}

impl<Backing: BackingData> core::fmt::Display for Compartment<Backing> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl<Backing: BackingData> Debug for Compartment<Backing> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compartment[{}]", self.name)
    }
}
