use std::ops::Index;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct PhysAddr(u64);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct PhysInfo {
    addr: PhysAddr,
}

pub struct DmaPinIter<'a> {
    pin: &'a [PhysInfo],
    idx: usize,
}

pub struct DmaPin<'a> {
    backing: &'a [PhysInfo],
}

impl<'a> DmaPin<'a> {
    pub(super) fn new(backing: &'a [PhysInfo]) -> Self {
        Self { backing }
    }
}

impl PhysInfo {
    pub fn new(addr: PhysAddr) -> Self {
        Self { addr }
    }

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
        // TODO: verify address
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
pub enum PinError {
    InternalError,
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
