use tracing::{debug, trace};

use crate::{tls::TlsRegion, DynlinkError};

use super::{Compartment, CompartmentRef};

impl Compartment {
    /// Build a useable TLS region, complete with copied templates, a control block, and a dtv.
    pub fn build_tls_region<T>(self: &CompartmentRef, tcb: T) -> Result<TlsRegion, DynlinkError> {
        self.with_inner_mut(|inner| {
            let alloc_layout = inner
                .tls_info
                .allocation_layout::<T>()
                .map_err(|_| DynlinkError::Unknown)?;
            debug!(
                "{}: building static TLS region (size: {}, align: {})",
                self,
                alloc_layout.size(),
                alloc_layout.align()
            );
            let base = unsafe { inner.alloc(alloc_layout) }.ok_or(DynlinkError::Unknown)?;

            let tls_region = inner.tls_info.allocate(self, base, tcb);
            trace!("{}: static TLS region: {:?}", self, tls_region);
            tls_region
        })
        .flatten()
    }
}
