use std::{alloc::Layout, ptr::NonNull};

use tracing::{debug, trace};

use super::Compartment;
use crate::{
    tls::{TlsInfo, TlsModId, TlsModule, TlsRegion},
    DynlinkError, DynlinkErrorKind,
};

impl Compartment {
    pub(crate) fn insert(&mut self, tm: TlsModule) -> TlsModId {
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

    /// Advance the TLS generation count by 1.
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
    pub fn build_tls_region<T>(
        &mut self,
        tcb: T,
        alloc: impl FnOnce(Layout) -> Option<NonNull<u8>>,
    ) -> Result<TlsRegion, DynlinkError> {
        let tls_info = self.tls_info.get(&self.tls_gen).ok_or_else(|| {
            DynlinkError::new(DynlinkErrorKind::NoTLSInfo {
                library: self.name.clone(),
            })
        })?;
        let alloc_layout = tls_info
            .allocation_layout::<T>()
            .map_err(DynlinkErrorKind::from)?;
        // Each compartment has its own libstd, so we can just all alloc directly.
        let base = alloc(alloc_layout).ok_or_else(|| DynlinkErrorKind::FailedToAllocate {
            comp: self.name.clone(),
            layout: alloc_layout,
        })?;
        debug!(
            "{}: building static TLS region (size: {}, align: {}) -> {:p}",
            self,
            alloc_layout.size(),
            alloc_layout.align(),
            base
        );

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
