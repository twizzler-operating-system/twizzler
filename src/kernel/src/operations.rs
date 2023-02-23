use alloc::vec::Vec;

use crate::{
    memory::context::{ContextRef, MappingPerms, ObjectContextInfo, UserContext},
    obj::ObjectRef,
};

pub fn map_object_into_context(
    slot: usize,
    obj: ObjectRef,
    vmc: ContextRef,
    perms: MappingPerms,
) -> Result<(), ()> {
    let r = vmc.insert_object(
        slot.try_into()?,
        &ObjectContextInfo::new(obj, perms, twizzler_abi::device::CacheType::WriteBack),
    );
    r.map_err(|_| ())
}

pub fn read_object(obj: &ObjectRef) -> Vec<u8> {
    let mut tree = obj.lock_page_tree();
    let mut v = alloc::vec![];
    let mut pn = 1.into();
    while let Some((p, _)) = tree.get_page(pn, false) {
        v.extend_from_slice(p.as_slice());
        pn = pn.next();
    }
    v
}
