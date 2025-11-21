use alloc::boxed::Box;

use twizzler_abi::object::ObjID;

use crate::{Cap, Gate, Revoc};

#[expect(dead_code)]

/// A Delegation, which can be used to delegate capabilities into other security contexts.
/// Currently not implemented
#[derive(Debug)]
pub struct Del {
    /// The receiver of this delegation
    pub receiver: ObjID,
    /// The provider of this delegation
    pub provider: ObjID,
    // mask:
    // flags:
    /// The gatemask, read about this in the paper
    gatemask: Gate,
    /// When this delegation is revoked
    revocation: Revoc,

    /// The signature for this delegation
    sig: heapless::Vec<u8, 1024>,
    /// Length of data
    datalen: u32,

    /// What this delegation holds
    inner: Option<Box<DelInner>>,
}

/// A delegation can hold a Delegation or a Capability
#[derive(Debug)]
pub enum DelInner {
    /// TODO: docs
    Delegation(Del),
    /// TODO: docs
    Capability(Cap),
}
