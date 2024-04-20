use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Default)]
#[repr(C)]
/// Specifications for an object-copy from a source object. The specified ranges are
/// source:[src_start, src_start + len) copied to <some unspecified destination object>:[dest_start,
/// dest_start + len). Each range must start within an object, and end within the object.
pub struct ObjectSource {
    /// The ID of the source object, or zero for filling destination with zero.
    pub id: ObjID,
    /// The offset into the source object to start the copy. If id is zero, this field is reserved
    /// for future use.
    pub src_start: u64,
    /// The offset into the dest object to start the copy or zero.
    pub dest_start: u64,
    /// The length of the copy or zero.
    pub len: usize,
}

impl ObjectSource {
    /// Construct a new ObjectSource.
    pub fn new_copy(id: ObjID, src_start: u64, dest_start: u64, len: usize) -> Self {
        Self {
            id,
            src_start,
            dest_start,
            len,
        }
    }

    /// Construct a new ObjectSource.
    pub fn new_zero(dest_start: u64, len: usize) -> Self {
        Self {
            id: 0.into(),
            src_start: 0,
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

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error returns for [sys_object_create].
pub enum ObjectCreateError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// A source or tie object was not found.
    #[error("source or tie object not found")]
    ObjectNotFound = 2,
    /// The kernel could not handle one of the source ranges.
    #[error("invalid source directive")]
    InvalidSource = 3,
}

impl core::error::Error for ObjectCreateError {}

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
