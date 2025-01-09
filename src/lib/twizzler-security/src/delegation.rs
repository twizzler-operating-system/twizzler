use alloc::boxed::Box;

use crate::{Cap, Gates, ObjectId, Revoc};

pub struct Del {
    pub receiver: ObjectId,
    pub provider: ObjectId,
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
