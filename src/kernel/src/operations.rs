use alloc::vec::Vec;

use twizzler_abi::{object::Protections, syscall::MapFlags};
use twizzler_rt_abi::{error::TwzError, object::ObjID};

use crate::{
    memory::context::{ContextRef, ObjectContextInfo, UserContext},
    obj::{ObjectRef, PageNumber},
};

pub fn map_object_into_context(
    slot: usize,
    obj: ObjectRef,
    vmc: ContextRef,
    perms: Protections,
    flags: MapFlags,
    target_sctx: ObjID,
) -> Result<(), TwzError> {
    vmc.insert_object(
        slot.try_into().map_err(|_| TwzError::INVALID_ARGUMENT)?,
        &ObjectContextInfo::new_in_sctx(
            obj,
            perms,
            twizzler_abi::device::CacheType::WriteBack,
            flags,
            target_sctx,
        ),
    )
}

pub fn read_object(obj: &ObjectRef) -> Vec<u8> {
    assert!(!obj.use_pager());
    let mut v = alloc::vec![];
    let mut offset = PageNumber::base_page().as_byte_offset();
    let mut tree = obj.lock_page_tables();
    while let Some(frame) = tree.get_frame(offset as u64) {
        v.extend_from_slice(frame.as_byte_slice());
        offset += frame.size();
    }
    v
}
