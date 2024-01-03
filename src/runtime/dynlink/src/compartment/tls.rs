use tracing::{debug, trace};

use crate::{library::BackingData, tls::TlsRegion, DynlinkError};

use super::Compartment;

impl<Backing: BackingData> Compartment<Backing> {
    /// Build a useable TLS region, complete with copied templates, a control block, and a dtv.
    pub fn build_tls_region<T>(&self, tcb: T) -> Result<TlsRegion, DynlinkError> {
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

        let tls_region = self.tls_info.allocate(self, base, tcb);
        trace!("{}: static TLS region: {:?}", self, tls_region);
        tls_region
    }
}
