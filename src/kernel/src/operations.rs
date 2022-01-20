use alloc::sync::Arc;

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
