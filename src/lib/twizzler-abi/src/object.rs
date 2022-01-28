//! Low-level object APIs, mostly around IDs and basic things like protection definitions and metadata.

use core::fmt::{LowerHex, UpperHex};

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// An object ID, represented as a transparent wrapper type. Any value where the upper 64 bits are
/// zero is invalid.
pub struct ObjID(u128);

impl ObjID {
    /// Create a new ObjID out of a 128 bit value.
    pub const fn new(id: u128) -> Self {
        Self(id)
    }

    /// Split an object ID into upper and lower values, useful for syscalls.
    pub fn split(&self) -> (u64, u64) {
        ((self.0 >> 64) as u64, (self.0 & 0xffffffffffffffff) as u64)
    }

    /// Build a new ObjID out of a high part and a low part.
    pub fn new_from_parts(hi: u64, lo: u64) -> Self {
        ObjID::new(((hi as u128) << 64) | (lo as u128))
    }
}

impl core::convert::AsRef<ObjID> for ObjID {
    fn as_ref(&self) -> &ObjID {
        self
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

bitflags::bitflags! {
    /// Mapping protections for mapping objects into the address space.
    pub struct Protections: u32 {
        /// Read allowed.
        const READ = 1;
        /// Write allowed.
        const WRITE = 2;
        /// Exec allowed.
        const EXEC = 4;
    }
}
