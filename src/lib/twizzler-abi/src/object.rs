//! Low-level object APIs, mostly around IDs and basic things like protection definitions and
//! metadata.

/// The maximum size of an object, including null page and meta page(s).
pub const MAX_SIZE: usize = 1024 * 1024 * 1024;
/// The size of the null page.
pub const NULLPAGE_SIZE: usize = 0x1000;

pub use twizzler_rt_abi::object::ObjID;
use twizzler_rt_abi::object::{MapError, MapFlags};

use crate::syscall::ObjectMapError;

bitflags::bitflags! {
    /// Mapping protections for mapping objects into the address space.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Protections: u32 {
        /// Read allowed.
        const READ = 1;
        /// Write allowed.
        const WRITE = 2;
        /// Exec allowed.
        const EXEC = 4;
    }
}

impl From<Protections> for MapFlags {
    fn from(p: Protections) -> Self {
        let mut f = MapFlags::empty();
        if p.contains(Protections::READ) {
            f.insert(MapFlags::READ);
        }

        if p.contains(Protections::WRITE) {
            f.insert(MapFlags::WRITE);
        }

        if p.contains(Protections::EXEC) {
            f.insert(MapFlags::EXEC);
        }
        f
    }
}

impl From<MapFlags> for Protections {
    fn from(value: MapFlags) -> Self {
        let mut f = Self::empty();
        if value.contains(MapFlags::READ) {
            f.insert(Protections::READ);
        }
        if value.contains(MapFlags::WRITE) {
            f.insert(Protections::WRITE);
        }
        if value.contains(MapFlags::EXEC) {
            f.insert(Protections::EXEC);
        }
        f
    }
}

impl From<MapFlags> for crate::syscall::MapFlags {
    fn from(_value: MapFlags) -> Self {
        Self::empty()
    }
}

impl Into<MapError> for ObjectMapError {
    fn into(self) -> MapError {
        match self {
            ObjectMapError::Unknown => MapError::Other,
            ObjectMapError::ObjectNotFound => MapError::NoSuchObject,
            ObjectMapError::InvalidSlot => MapError::Other,
            ObjectMapError::InvalidProtections => MapError::PermissionDenied,
            ObjectMapError::InvalidArgument => MapError::InvalidArgument,
        }
    }
}
