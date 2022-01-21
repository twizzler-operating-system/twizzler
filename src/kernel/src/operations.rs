use alloc::{sync::Arc, vec::Vec};

use crate::{
    memory::context::{Mapping, MappingPerms, MemoryContextRef},
    obj::ObjectRef,
};

pub fn map_object_into_context(
    slot: usize,
    obj: ObjectRef,
    vmc: MemoryContextRef,
    perms: MappingPerms,
) -> Result<(), ()> {
    let mapping = Arc::new(Mapping::new(obj.clone(), vmc.clone(), slot, perms));
    let mut vmc = vmc.lock();
    obj.insert_mapping(mapping.clone());
    vmc.insert_mapping(mapping);

    Ok(())
}

pub fn read_object(obj: &ObjectRef) -> Vec<u8> {
    let mut tree = obj.lock_page_tree();
    let mut v = alloc::vec![];
    let mut pn = 1.into();
    loop {
        if let Some((p, _)) = tree.get_page(pn, false) {
            v.extend_from_slice(p.as_slice());
        } else {
            break;
        }
        pn = pn.next();
    }
    v
}
