use std::ptr::NonNull;

use tracing::{debug, error, trace};

use crate::{
    compartment::CompartmentAlloc,
    tls::{TlsModule, TlsRegion},
    DynlinkError,
};

use super::CompartmentInner;

impl CompartmentInner {
    pub fn build_tls_region<T>(&mut self, tcb: T) -> Result<TlsRegion, DynlinkError> {
        let alloc_layout = self
            .tls_info
            .allocation_layout::<T>()
            .map_err(|_| DynlinkError::Unknown)?;
        debug!(
            "{}: building static TLS region (size: {}, align: {})",
            self,
            alloc_layout.size(),
            alloc_layout.align()
        );
        let base = unsafe { self.alloc(alloc_layout) }.ok_or(DynlinkError::Unknown)?;
        let tls_region = self.tls_info.allocate(base, tcb)?;
        trace!("{}: static TLS region: {:?}", self, tls_region);
        return Ok(tls_region);
    }
}
