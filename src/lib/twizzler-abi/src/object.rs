//! Low-level object APIs, mostly around IDs and basic things like protection definitions and metadata.

use core::{
    fmt::{LowerHex, UpperHex},
    marker::PhantomData,
};

use crate::syscall::{MapFlags, ObjectCreate, ObjectCreateFlags};

pub const MAX_SIZE: usize = 1024 * 1024 * 1024;
pub const NULLPAGE_SIZE: usize = 0x1000;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

impl core::fmt::Debug for ObjID {
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

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct InternalObject<T> {
    slot: usize,
    id: ObjID,
    _pd: PhantomData<T>,
}

impl<T> InternalObject<T> {
    #[allow(dead_code)]
    pub(crate) fn base(&self) -> &T {
        let (start, _) = crate::slot::to_vaddr_range(self.slot);
        unsafe { (start as *const T).as_ref().unwrap() }
    }

    #[allow(dead_code)]
    pub(crate) fn id(&self) -> ObjID {
        self.id
    }

    #[allow(dead_code)]
    pub(crate) fn slot(&self) -> usize {
        self.slot
    }

    #[allow(dead_code)]
    pub(crate) fn create_data_and_map() -> Option<Self> {
        let cs = ObjectCreate::new(
            crate::syscall::BackingType::Normal,
            crate::syscall::LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        );
        let id = crate::syscall::sys_object_create(cs, &[], &[]).ok()?;

        let slot = crate::slot::global_allocate()?;

        crate::syscall::sys_object_map(
            None,
            id,
            slot,
            Protections::READ | Protections::WRITE,
            MapFlags::empty(),
        )
        .ok()?;

        //TODO: delete
        Some(Self {
            id,
            slot,
            _pd: PhantomData,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn map(id: ObjID, prot: Protections) -> Option<Self> {
        let slot = crate::slot::global_allocate()?;
        crate::syscall::sys_object_map(None, id, slot, prot, MapFlags::empty()).ok()?;
        Some(Self {
            id,
            slot,
            _pd: PhantomData,
        })
    }

    #[allow(dead_code)]
    pub(crate) unsafe fn offset_from_base<D>(&self, offset: usize) -> &mut D {
        let (start, _) = crate::slot::to_vaddr_range(self.slot);
        ((start + offset) as *mut D).as_mut().unwrap()
    }
}

impl<T> Drop for InternalObject<T> {
    fn drop(&mut self) {
        crate::slot::global_release(self.slot);
    }
}
