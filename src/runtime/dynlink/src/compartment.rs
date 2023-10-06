use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
};

use talc::{ErrOnOom, Talc};
use twizzler_object::Object;

use crate::{tls::TlsInfo, DynlinkError};

mod alloc;
mod initialize;
mod load;
mod tls;

pub(crate) struct CompartmentInner {
    name: String,
    id: u128,
    allocator: Talc<ErrOnOom>,
    alloc_objects: Vec<Object<u8>>,
    pub(crate) tls_info: TlsInfo,
}

pub struct Compartment {
    name: String,
    inner: Mutex<CompartmentInner>,
}

impl PartialEq for CompartmentInner {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for CompartmentInner {}

impl PartialOrd for CompartmentInner {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for CompartmentInner {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

impl core::fmt::Display for CompartmentInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
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

pub type CompartmentRef = Arc<Compartment>;

impl CompartmentInner {
    pub(crate) fn new(name: String, id: u128) -> Self {
        Self {
            name,
            id,
            allocator: Talc::new(ErrOnOom),
            alloc_objects: vec![],
            tls_info: Default::default(),
        }
    }
}

#[allow(dead_code)]
impl Compartment {
    pub(crate) fn new(name: String, id: u128) -> Self {
        Self {
            name: name.clone(),
            inner: Mutex::new(CompartmentInner::new(name, id)),
        }
    }

    pub(crate) fn with_inner_mut<R>(
        &self,
        f: impl FnOnce(&mut CompartmentInner) -> R,
    ) -> Result<R, DynlinkError> {
        Ok(f(&mut *self.inner.lock()?))
    }

    pub(crate) fn with_inner<R>(
        &self,
        f: impl FnOnce(&CompartmentInner) -> R,
    ) -> Result<R, DynlinkError> {
        Ok(f(&*self.inner.lock()?))
    }
}
