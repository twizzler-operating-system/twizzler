use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use talc::{ErrOnOom, Talc};
use twizzler_object::Object;

use crate::{
    library::LibraryRef,
    symbol::RelocatedSymbol,
    tls::{TlsInfo, TlsRegion},
    DynlinkError,
};

mod alloc;
mod initialize;
mod load;
mod relocate;
mod tls;

pub(crate) use self::alloc::CompartmentAlloc;

pub(crate) struct CompartmentInner {
    name: String,
    id: u128,
    name_map: HashMap<String, LibraryRef>,
    allocator: Talc<ErrOnOom>,
    alloc_objects: Vec<Object<u8>>,
    pub(crate) tls_info: TlsInfo,
}

pub struct Compartment {
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
        write!(f, "{}", self.inner.lock().unwrap().name)
    }
}

pub type CompartmentRef = Arc<Compartment>;

impl CompartmentInner {
    pub(crate) fn new(name: String, id: u128) -> Self {
        Self {
            name,
            id,
            name_map: Default::default(),
            allocator: Talc::new(ErrOnOom),
            alloc_objects: vec![],
            tls_info: Default::default(),
        }
    }
}

impl Compartment {
    pub(crate) fn new(name: String, id: u128) -> Self {
        Self {
            inner: Mutex::new(CompartmentInner::new(name, id)),
        }
    }

    pub fn build_tls_region<T>(&self, tcb: T) -> Result<TlsRegion, DynlinkError> {
        self.inner.lock()?.build_tls_region(tcb)
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
