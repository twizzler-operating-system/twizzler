use twizzler::object::Object;
use twizzler_abi::upcall::ObjectMemoryFaultInfo;

const MAX_SEC_CTX_MAP_LEN: u8 = 100;

pub struct SecCtxMap {
    map: [CtxMapItem; MAX_SEC_CTX_MAP_LEN],
    len: u32,
}

pub struct CtxMapItem {
    target_id: ObjId,
    item_type: CtxMapItemType,
    len: u32,
    offset: u32,
}

pub enum CtxMapItemType {
    Cap,
    Del,
}

impl SecCtxMap {
    pub fn new() -> Self {
        //TODO: this would be parsed from a object handle, have no clue how to access an object
        // handle though
    }

    //TODO:
    // insert
    // remove
    // lookup
}
