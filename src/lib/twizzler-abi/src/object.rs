use core::fmt::{LowerHex, UpperHex};

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjID(u128);

impl ObjID {
    pub fn new(id: u128) -> Self {
        Self(id)
    }

    pub fn split(&self) -> (u64, u64) {
        ((self.0 >> 64) as u64, (self.0 & 0xffffffffffffffff) as u64)
    }
}

impl From<u128> for ObjID {
    fn from(id: u128) -> Self {
        Self::new(id)
    }
}

impl LowerHex for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl UpperHex for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:X}", self.0)
    }
}

impl core::fmt::Display for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ObjID({:x})", self.0)
    }
}

pub fn objid_from_parts(hi: u64, lo: u64) -> ObjID {
    ObjID::new(((hi as u128) << 64) | (lo as u128))
}

bitflags::bitflags! {
    pub struct Protections: u32 {
        const READ = 1;
        const WRITE = 2;
        const EXEC = 4;
    }
}
