use std::ops::Index;

use crate::arch::DMA_PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// A physical address. Must be aligned on [DMA_PAGE_SIZE].
pub struct PhysAddr(u64);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Information about a page of DMA memory, including it's physical address.
pub struct PhysInfo {
    addr: PhysAddr,
}

/// An iterator over DMA memory pages, returning [PhysInfo].
pub struct DmaPinIter<'a> {
    pin: &'a [PhysInfo],
    idx: usize,
}

/// A representation of some pinned memory for a region.
pub struct DmaPin<'a> {
    backing: &'a [PhysInfo],
}

impl<'a> DmaPin<'a> {
    pub(super) fn new(backing: &'a [PhysInfo]) -> Self {
        Self { backing }
    }
}

impl PhysInfo {
    pub(crate) fn new(addr: PhysAddr) -> Self {
        Self { addr }
    }

    /// Get the address of this DMA memory page.
    pub fn addr(&self) -> PhysAddr {
        self.addr
    }
}

impl From<PhysAddr> for u64 {
    fn from(p: PhysAddr) -> Self {
        p.0
    }
}

impl TryFrom<u64> for PhysAddr {
    type Error = ();

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value & (DMA_PAGE_SIZE as u64 - 1) != 0 {
            return Err(());
        }
        Ok(Self(value))
    }
}

impl<'a> IntoIterator for DmaPin<'a> {
    type Item = PhysInfo;

    type IntoIter = DmaPinIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        DmaPinIter {
            pin: self.backing,
            idx: 0,
        }
    }
}

impl<'a> Iterator for DmaPinIter<'a> {
    type Item = PhysInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.pin.len() {
            None
        } else {
            let ret = self.pin[self.idx];
            self.idx += 1;
            Some(ret)
        }
    }
}

impl<'a> Index<usize> for DmaPin<'a> {
    type Output = PhysInfo;

    fn index(&self, index: usize) -> &Self::Output {
        &self.backing[index]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Possible failure modes for pinning memory.
pub enum PinError {
    /// An internal error occurred.
    InternalError,
    /// Kernel resources are exhausted.
    Exhausted,
}

#[cfg(test)]
mod tests {
    use twizzler_abi::syscall::{BackingType, LifetimeType};
    use twizzler_object::{CreateSpec, Object};

    use crate::dma::{Access, DmaObject, DmaOptions};

    fn make_object() -> Object<()> {
        let spec = CreateSpec::new(LifetimeType::Volatile, BackingType::Normal);
        Object::create_with(&spec, |_| {}).unwrap()
    }
    #[test]
    fn pin_kaction() {
        let dma = DmaObject::new(make_object());
        let mut reg = dma.region::<u32>(Access::BiDirectional, DmaOptions::default());
        let pin = reg.pin().unwrap();
        for phys in pin {
            let addr = phys.addr();
            let addr: u64 = addr.into();
            assert!(addr & 0xfff == 0);
            assert_ne!(addr, 0);
        }
    }
}
