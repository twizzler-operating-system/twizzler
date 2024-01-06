use tracing::{debug, trace};

use crate::{
    library::BackingData,
    tls::{TlsInfo, TlsModId, TlsModule, TlsRegion},
    DynlinkError, DynlinkErrorKind,
};

use super::Compartment;

impl<Backing: BackingData> Compartment<Backing> {
    pub fn insert(&mut self, tm: TlsModule) -> TlsModId {
        let prev_gen = self.tls_gen;
        self.tls_gen += 1;
        let prev_info = self
            .tls_info
            .remove(&prev_gen)
            .unwrap_or_else(|| TlsInfo::new(self.tls_gen));
        let mut next = prev_info.clone_to_new_gen(self.tls_gen);
        let id = next.insert(tm);
        self.tls_info.insert(self.tls_gen, next);
        id
    }

    pub fn advance_tls_generation(&mut self) -> u64 {
        let tng = self.tls_gen + 1;
        let initial = if let Some(prev) = self.tls_info.get(&self.tls_gen) {
            prev.clone_to_new_gen(tng)
        } else {
            TlsInfo::new(tng)
        };
        self.tls_info.insert(tng, initial);
        tng
    }

    /// Build a useable TLS region, complete with copied templates, a control block, and a dtv.
    pub fn build_tls_region<T>(&mut self, tcb: T) -> Result<TlsRegion, DynlinkError> {
        let tls_info = self.tls_info.get(&self.tls_gen).ok_or_else(|| {
            DynlinkError::new(DynlinkErrorKind::NoTLSInfo {
                library: self.name.clone(),
            })
        })?;
        let alloc_layout = tls_info
            .allocation_layout::<T>()
            .map_err(|e| DynlinkErrorKind::from(e))?;
        debug!(
            "{}: building static TLS region (size: {}, align: {})",
            self,
            alloc_layout.size(),
            alloc_layout.align()
        );
        let base = unsafe { self.alloc(alloc_layout) }.ok_or_else(|| {
            DynlinkErrorKind::FailedToAllocate {
                comp: self.name.clone(),
                layout: alloc_layout,
            }
        })?;

        let tls_info = self.tls_info.get(&self.tls_gen).ok_or_else(|| {
            DynlinkError::new(DynlinkErrorKind::NoTLSInfo {
                library: self.name.clone(),
            })
        })?;
        let tls_region = tls_info.allocate(self, base, tcb);
        trace!("{}: static TLS region: {:?}", self, tls_region);
        tls_region
    }
}
