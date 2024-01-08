//! Compartments are an abstraction for isolation of library components, but they are not done yet.

use petgraph::stable_graph::NodeIndex;
use std::{collections::HashMap, fmt::Debug};

use talc::{ErrOnOom, Talc};

use crate::{
    library::{BackingData, Library},
    tls::TlsInfo,
};

mod alloc;
mod tls;

pub struct Compartment<Backing: BackingData> {
    pub name: String,
    pub(crate) library_names: HashMap<String, NodeIndex>,
    pub(super) allocator: Talc<ErrOnOom>,
    pub(super) alloc_objects: Vec<Backing>,
    pub(crate) tls_info: HashMap<u64, TlsInfo>,
    pub(crate) tls_gen: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(transparent)]
pub struct CompartmentId(pub(crate) usize);

unsafe impl<B: BackingData> Sync for Compartment<B> {}

impl<Backing: BackingData> Compartment<Backing> {
    pub fn new(name: String) -> Self {
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
