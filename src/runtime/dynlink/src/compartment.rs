//! Compartments are an abstraction for isolation of library components, but they are not done yet.

use std::fmt::Debug;

use talc::{ErrOnOom, Talc};

use crate::library::BackingData;

mod alloc;
mod tls;

pub struct Compartment<Backing: BackingData> {
    pub name: String,
    pub(super) allocator: Talc<ErrOnOom>,
    pub(super) alloc_objects: Vec<Backing>,
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
