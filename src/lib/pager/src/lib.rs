use secgate::{util::Descriptor, TwzError};
use twizzler_rt_abi::object::ObjID;

#[secgate::gatecall]
pub fn pager_start(q1: ObjID, q2: ObjID) -> Result<ObjID, TwzError> {}

#[secgate::gatecall]
pub fn adv_lethe() -> Result<(), TwzError> {}

#[secgate::gatecall]
pub fn disk_len(id: ObjID) -> Result<u64, TwzError> {}

#[secgate::gatecall]
pub fn pager_open_handle() -> Result<(Descriptor, ObjID), TwzError> {}
#[secgate::gatecall]
pub fn pager_close_handle(desc: Descriptor) -> Result<(), TwzError> {}
#[secgate::gatecall]
pub fn pager_enumerate_external(
    desc: Descriptor,
    id: ObjID,
    skip: usize,
    count: usize,
) -> Result<usize, TwzError> {
}
#[secgate::gatecall]
pub fn pager_lookup_external(
    desc: Descriptor,
    id: ObjID,
    namelen: usize,
) -> Result<usize, TwzError> {
}
#[secgate::gatecall]
pub fn pager_create_external(
    desc: Descriptor,
    dir: ObjID,
    mode: libc::mode_t,
    namelen: usize,
    link_to: Option<ObjID>,
) -> Result<usize, TwzError> {
}

#[secgate::gatecall]
pub fn pager_unlink_external(desc: Descriptor, id: ObjID, namelen: usize) -> Result<(), TwzError> {}

#[secgate::gatecall]
pub fn pager_readlink_external(desc: Descriptor, id: ObjID) -> Result<usize, TwzError> {}
