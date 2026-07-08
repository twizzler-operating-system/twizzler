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
    /// The `ObjID` of the `SecCtx` providing these permissions.
    pub ctx: ObjID,
    /// The `Protections` being provided.
    pub provide: Protections,
    /// The `Protections` being restricted.
    pub restrict: Protections,
}

impl PermsInfo {
    /// Create a new instance of `PermsInfo`.
    pub fn new(ctx: ObjID, provide: Protections, restrict: Protections) -> Self {
        Self {
            ctx,
            provide,
            restrict,
        }
    }

    /// Calculate the effective protections for an object given the default protections and the
    /// protections on the mapping.
    pub fn effective(&self, default_protect: Protections, map_prots: Protections) -> Protections {
        (default_protect | self.provide) & !self.restrict & map_prots
    }
}
