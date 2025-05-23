use twizzler_abi::object::{ObjID, Protections};

mod base;
pub use base::*;

#[cfg(feature = "user")]
mod user;

#[cfg(feature = "user")]
pub use user::*;

/// Information about protections for a given object within a context.
#[derive(Clone, Copy, Debug)]
pub struct PermsInfo {
    ctx: ObjID,
    prot: Protections,
}
