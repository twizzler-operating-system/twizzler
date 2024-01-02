//! Compartments are an abstraction for isolation of library components, but they are not done yet.

use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
};

use talc::{ErrOnOom, Talc};
use twizzler_object::Object;

use crate::{library::BackingData, tls::TlsInfo, DynlinkError};

mod alloc;
mod load;
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

impl<Backing: BackingData> Compartment<Backing> {
    pub(crate) fn new(name: String, id: u128) -> Self {
        todo!()
    }
}
