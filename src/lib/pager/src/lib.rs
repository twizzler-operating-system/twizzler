use twizzler_rt_abi::object::ObjID;

#[link(name = "pager_srv")]
extern "C" {}

pub fn pager_start(q1: ObjID, q2: ObjID) {
    pager_srv::pager_start(q1, q2).ok().unwrap();
}
