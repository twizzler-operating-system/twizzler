//! Low-level object APIs, mostly around IDs and basic things like protection definitions and
//! metadata.

/// The maximum size of an object, including null page and meta page(s).
pub const MAX_SIZE: usize = 1024 * 1024 * 1024;
/// The size of the null page.
pub const NULLPAGE_SIZE: usize = 0x1000;

pub use twizzler_rt_abi::object::ObjID;

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

#[cfg(feature = "runtime")]
pub(crate) use crate::runtime::object::InternalObject;
