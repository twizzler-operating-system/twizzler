use twizzler_abi::object::ObjID;

use crate::{
    obj::{lookup_object, LookupFlags},
    queue::QueueObject,
};

pub fn init_pager_queue(id: ObjID, outgoing: bool) {
    let obj = match lookup_object(id, LookupFlags::empty()) {
        crate::obj::LookupResult::Found(o) => o,
        _ => panic!("pager queue not found"),
    };
    let queue = QueueObject::<(), ()>::from_object(obj);
}
