use alloc::{sync::Arc, vec::Vec};

use crate::{
    memory::context::{ContextRef, MappingPerms},
    obj::ObjectRef,
};

pub fn map_object_into_context(
    slot: usize,
    obj: ObjectRef,
    vmc: ContextRef,
    perms: MappingPerms,
) -> Result<(), ()> {
    todo!()
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
