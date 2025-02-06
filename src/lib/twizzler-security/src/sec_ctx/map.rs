use twizzler::object::{Object, RawObject};
use twizzler_abi::{marker::BaseType, object::ObjID};
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
    pub fn parse(sec_ctx_id: ObjID) -> *mut Self {
        let obj = Object::<SecCtxMap>::map(sec_ctx_id, MapFlags::READ).unwrap();
        obj.base_mut_ptr::<SecCtxMap>()
    }

    /// inserts a CtxMapItemType into the SecCtxMap and returns the write offset into the object
    pub fn insert(ptr: *mut Self, target_id: ObjID, item_type: CtxMapItemType, len: u32) -> u32 {
        unsafe {
            let mut ap = *ptr;

            //TODO: need to actually calculate this out / worry about allocation strategies
            let write_offset = ap.len * len + size_of::<SecCtxMap>() as u32;
            ap.map[len as usize] = CtxMapItem {
                target_id,
                item_type,
                len,
                offset: write_offset,
            };
            ap.len += 1;

            return write_offset;
        }
    }

    //TODO:
    // insert
    // remove
    // lookup
}

// impl BaseType for SecCtxMap{
//     fn init<T>(_t: T) -> Self {
//         SecCtxMap::parse(_t)
//     }

//      fn tags() -> &'static [(twizzler_abi::marker::BaseVersion, twizzler_abi::marker::BaseTag)] {

//      }
// }
