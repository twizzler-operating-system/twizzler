use core::fmt;

use crate::{object::ObjID, arch::syscall::raw_syscall};
use bitflags::bitflags;

use super::{Syscall, convert_codes_to_result};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Specifications for an object-copy from a source object. The specified ranges are
/// source:[src_start, src_start + len) copied to <some unspecified destination object>:[dest_start,
/// dest_start + len). Each range must start within an object, and end within the object.
pub struct ObjectSource {
    /// The ID of the source object.
    pub id: ObjID,
    /// The offset into the source object to start the copy.
    pub src_start: u64,
    /// The offset into the dest object to start the copy to.
    pub dest_start: u64,
    /// The length of the copy.
    pub len: usize,
}

impl ObjectSource {
    /// Construct a new ObjectSource.
    pub fn new(id: ObjID, src_start: u64, dest_start: u64, len: usize) -> Self {
        Self {
            id,
            src_start,
            dest_start,
            len,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// The backing memory type for this object. Currently doesn't do anything.
pub enum BackingType {
    /// The default, let the kernel decide based on the [LifetimeType] of the object.
    Normal = 0,
}

impl Default for BackingType {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// The base lifetime type of the object. Note that this does not ensure that the object is stored
/// in a specific type of memory, the kernel is allowed to migrate objects with the Normal
/// [BackingType] as it sees fit. For more information on object lifetime, see [the book](https://twizzler-operating-system.github.io/nightly/book/object_lifetime.html).
pub enum LifetimeType {
    /// This object is volatile, and is expected to be deleted after a power cycle.
    Volatile = 0,
    /// This object is persistent, and should be deleted only after an explicit delete call.
    Persistent = 1,
}

bitflags! {
    /// Flags to pass to the object create system call.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)] 
    pub struct ObjectCreateFlags: u32 {
    }
}

bitflags! {
    /// Flags controlling how a particular object tie operates.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct CreateTieFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Full object creation specification, minus ties.
pub struct ObjectCreate {
    kuid: ObjID,
    bt: BackingType,
    lt: LifetimeType,
    flags: ObjectCreateFlags,
}
impl ObjectCreate {
    /// Build a new object create specification.
    pub fn new(
        bt: BackingType,
        lt: LifetimeType,
        kuid: Option<ObjID>,
        flags: ObjectCreateFlags,
    ) -> Self {
        Self {
            kuid: kuid.unwrap_or_else(|| 0.into()),
            bt,
            lt,
            flags,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// A specification of ties to create.
/// (see [the book](https://twizzler-operating-system.github.io/nightly/book/object_lifetime.html) for more information on ties).
pub struct CreateTieSpec {
    id: ObjID,
    flags: CreateTieFlags,
}

impl CreateTieSpec {
    /// Create a new CreateTieSpec.
    pub fn new(id: ObjID, flags: CreateTieFlags) -> Self {
        Self { id, flags }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error returns for [sys_object_create].
pub enum ObjectCreateError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.
    InvalidArgument = 1,
    /// A source or tie object was not found.
    ObjectNotFound = 2,
    /// The kernel could not handle one of the source ranges.
    SourceMisalignment = 3,
}

impl ObjectCreateError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "an argument was invalid",
            Self::ObjectNotFound => "a referenced object was not found",
            Self::SourceMisalignment => "a source specification had an unsatisfiable range",
        }
    }
}

impl From<ObjectCreateError> for u64 {
    fn from(x: ObjectCreateError) -> Self {
        x as Self
    }
}

impl From<u64> for ObjectCreateError {
    fn from(x: u64) -> Self {
        match x {
            3 => Self::SourceMisalignment,
            2 => Self::ObjectNotFound,
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectCreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectCreateError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

/// Create an object, returning either its ID or an error.
pub fn sys_object_create(
    create: ObjectCreate,
    sources: &[ObjectSource],
    ties: &[CreateTieSpec],
) -> Result<ObjID, ObjectCreateError> {
    let args = [
        &create as *const ObjectCreate as u64,
        sources.as_ptr() as u64,
        sources.len() as u64,
        ties.as_ptr() as u64,
        ties.len() as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCreate, &args) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        crate::object::ObjID::new_from_parts,
        |_, v| ObjectCreateError::from(v),
    )
}
