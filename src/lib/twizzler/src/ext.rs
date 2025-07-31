use twizzler_abi::meta::{MetaExt, MetaExtTag};
use twizzler_rt_abi::object::ObjectHandle;

use crate::{
    object::RawObject,
    ptr::{InvPtr, Ref},
};

pub mod twzio;

pub trait MetaExtension {
    type Data;
    const TAG: MetaExtTag;

    fn get_data<'a>(handle: &'a ObjectHandle, ext: &MetaExt) -> Option<Ref<'a, Self::Data>> {
        if ext.tag != Self::TAG {
            return None;
        }

        // TODO: support external pointers for meta extensions?
        let ptr = handle.lea(ext.value, size_of::<Self::Data>())?;
        let r = unsafe { Ref::from_raw_parts(ptr, handle) };
        Some(r)
    }
}
