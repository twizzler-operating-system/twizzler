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
    pub ctx_id: ObjID,
    pub prot: Protections,
}

impl PermsInfo {
    pub fn new(ctx_id: ObjID, prot: Protections) -> Self {
        Self { ctx_id, prot }
    }
}
