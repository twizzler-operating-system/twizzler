use twizzler_rt_abi::object::ObjID;

#[link(name = "pager_srv")]
extern "C" {}

pub fn pager_start(q1: ObjID, q2: ObjID) {
    pager_srv::pager_start(q1, q2).ok().unwrap();
}

pub fn sync_object(id: ObjID) {
    pager_srv::full_object_sync(id).unwrap();
}

pub fn show_lethe() {
    pager_srv::show_lethe();
}

pub fn adv_lethe() {
    pager_srv::adv_lethe();
}
