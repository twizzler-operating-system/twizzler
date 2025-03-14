use secgate::util::{Descriptor, Handle, SimpleBuffer};
use twizzler_rt_abi::object::{MapFlags, ObjID};

#[link(name = "pager_srv")]
extern "C" {}

pub fn pager_start(q1: ObjID, q2: ObjID) {
    pager_srv::pager_start(q1, q2).ok().unwrap();
}

pub fn sync_object(id: ObjID) {
    pager_srv::full_object_sync(id).unwrap();
}

pub fn adv_lethe() {
    pager_srv::adv_lethe();
}
