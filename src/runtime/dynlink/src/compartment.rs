use std::{collections::HashMap, sync::Arc};

use talc::{ErrOnOom, Talc};
use twizzler_object::Object;

use crate::{library::LibraryRef, symbol::RelocatedSymbol, DynlinkError};

mod alloc;
mod initialize;
mod load;
mod relocate;

pub struct Compartment {
    name: String,
    id: u128,
    name_map: HashMap<String, LibraryRef>,
    allocator: Talc<ErrOnOom>,
    alloc_objects: Vec<Object<u8>>,
}

impl PartialEq for Compartment {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Compartment {}

impl PartialOrd for Compartment {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Compartment {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

impl core::fmt::Display for Compartment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

pub type CompartmentRef = Arc<Compartment>;

impl Compartment {
    pub fn lookup_symbol(&self, name: &str) -> Result<RelocatedSymbol, DynlinkError> {
        todo!()
    }
}
