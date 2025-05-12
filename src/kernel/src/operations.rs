use alloc::vec::Vec;

use twizzler_abi::object::Protections;

use crate::{
    memory::context::{ContextRef, ObjectContextInfo, UserContext},
    obj::{range::PageStatus, ObjectRef},
};

pub fn map_object_into_context(
    slot: usize,
    obj: ObjectRef,
    vmc: ContextRef,
    perms: Protections,
) -> Result<(), ()> {
    let r = vmc.insert_object(
        slot.try_into()?,
        &ObjectContextInfo::new(obj, perms, twizzler_abi::device::CacheType::WriteBack),
    );
    r.map_err(|_| ())
}

pub fn read_object(obj: &ObjectRef) -> Vec<u8> {
    assert!(!obj.use_pager());
    let mut tree = obj.lock_page_tree();
    let mut v = alloc::vec![];
    let mut pn = 1.into();
    while let PageStatus::Ready(p, _) = tree.get_page(pn, false, None) {
        v.extend_from_slice(p.as_slice());
        pn = pn.next();
    }
    v
}
