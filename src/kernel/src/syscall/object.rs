use alloc::sync::Arc;
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{CreateTieSpec, ObjectCreate, ObjectCreateError, ObjectMapError, ObjectSource},
};
use x86_64::VirtAddr;

use crate::{
    obj::{copy::CopySpec, LookupFlags, Object, PageNumber},
    thread::current_memory_context,
};

pub fn sys_object_create(
    _create: &ObjectCreate,
    srcs: &[ObjectSource],
    _ties: &[CreateTieSpec],
) -> Result<ObjID, ObjectCreateError> {
    let obj = Arc::new(Object::new());
    for src in srcs {
        let so = crate::obj::lookup_object(src.id, LookupFlags::empty())
            .ok_or(ObjectCreateError::ObjectNotFound)?;
        let cs = CopySpec::new(
            so,
            PageNumber::from_address(VirtAddr::new(src.src_start)),
            PageNumber::from_address(VirtAddr::new(src.dest_start)),
            src.len,
        );
        crate::obj::copy::copy_ranges(&cs.src, cs.src_start, &obj, cs.dest_start, cs.length)
    }
    crate::obj::register_object(obj.clone());
    Ok(obj.id())
}

pub fn sys_object_map(id: ObjID, slot: usize, prot: Protections) -> Result<usize, ObjectMapError> {
    let vm = current_memory_context().unwrap();
    let obj = crate::obj::lookup_object(id, LookupFlags::empty());
    let obj = match obj {
        crate::obj::LookupResult::NotFound => return Err(ObjectMapError::ObjectNotFound),
        crate::obj::LookupResult::WasDeleted => return Err(ObjectMapError::ObjectNotFound),
        crate::obj::LookupResult::Pending => return Err(ObjectMapError::ObjectNotFound),
        crate::obj::LookupResult::Found(obj) => obj,
    };
    // TODO
    let _res = crate::operations::map_object_into_context(slot, obj, vm, prot.into());
    Ok(slot)
}
