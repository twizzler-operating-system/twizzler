use twizzler::object::{Object, RawObject};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

const MAX_SEC_CTX_MAP_LEN: u8 = 100;

#[derive(Clone, Copy)]
pub struct SecCtxMap {
    map: [CtxMapItem; MAX_SEC_CTX_MAP_LEN as usize],
    len: u32,
}

#[derive(Clone, Copy)]
pub struct CtxMapItem {
    target_id: ObjID,
    item_type: CtxMapItemType,
    len: u32,
    offset: u32,
}

#[derive(Clone, Copy)]
pub enum CtxMapItemType {
    Cap,
    Del,
}

impl SecCtxMap {
    pub fn new(sec_ctx_id: ObjID) -> Self {
        let obj = Object::<SecCtxMap>::map(sec_ctx_id, MapFlags::READ).unwrap();
        let ptr = obj.base_ptr::<SecCtxMap>();
        unsafe {
            let map = *ptr;
            return map;
        }
    }

    //TODO:
    // insert
    // remove
    // lookup
}
