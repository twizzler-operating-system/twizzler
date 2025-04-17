use alloc::boxed::Box;

use twizzler_abi::object::ObjID;

use crate::{Cap, Gates, Revoc};

pub struct Del {
    pub receiver: ObjID,
    pub provider: ObjID,
    // mask:
    // flags:
    gatemask: Gates,
    revocation: Revoc,
    siglen: u16,
    datalen: u32,
    inner: Option<Box<DelInner>>,
    sig: [u8; 1024],
}

pub enum DelInner {
    Delegation(Del),
    Capability(Cap),
}
