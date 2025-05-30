use alloc::vec::Vec;

use twizzler_abi::{object::Protections, syscall::MapFlags};
use twizzler_rt_abi::error::TwzError;

use crate::{
    memory::context::{ContextRef, ObjectContextInfo, UserContext},
    obj::{
        range::{GetPageFlags, PageStatus},
        ObjectRef,
    },
};

pub fn map_object_into_context(
    slot: usize,
    obj: ObjectRef,
    vmc: ContextRef,
    perms: Protections,
    flags: MapFlags,
) -> Result<(), TwzError> {
    vmc.insert_object(
        slot.try_into().map_err(|_| TwzError::INVALID_ARGUMENT)?,
        &ObjectContextInfo::new(
            obj,
            perms,
            twizzler_abi::device::CacheType::WriteBack,
            flags,
        ),
    )
}

pub fn read_object(obj: &ObjectRef) -> Vec<u8> {
    assert!(!obj.use_pager());
    let mut tree = obj.lock_page_tree();
    let mut v = alloc::vec![];
    let mut pn = 1.into();
    while let PageStatus::Ready(p, _) = tree.get_page(pn, GetPageFlags::empty(), None) {
        v.extend_from_slice(p.as_slice());
        pn = pn.next();
    }
    v
}
