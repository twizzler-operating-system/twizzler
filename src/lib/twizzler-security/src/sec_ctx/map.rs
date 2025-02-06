use twizzler::{
    collections::vec::Vec,
    marker::{BaseType, Invariant, StoreCopy},
    object::{Object, RawObject},
};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

const MAX_SEC_CTX_MAP_LEN: usize = 5;

#[derive(Clone, Copy)]
pub struct SecCtxMap {
    map: [CtxMapItem; MAX_SEC_CTX_MAP_LEN as usize],
    len: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct CtxMapItem {
    target_id: ObjID,
    item_type: CtxMapItemType,
    len: u32,
    offset: u32,
}

#[derive(Clone, Copy, Debug)]
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
            let mut map = *ptr;

            //TODO: need to actually calculate this out / worry about allocation strategies
            let write_offset = map.len * len + size_of::<SecCtxMap>() as u32;
            map.map[map.len as usize] = CtxMapItem {
                target_id,
                item_type,
                len,
                offset: write_offset,
            };
            map.len += 1;

            return write_offset;
        }
    }

    pub fn new() -> Self {
        Self {
            map: [CtxMapItem {
                target_id: 0.into(),
                item_type: CtxMapItemType::Del,
                len: 0,
                offset: 0,
            }; MAX_SEC_CTX_MAP_LEN as usize],
            len: 0,
        }
    }

    // size && array of items
    pub fn lookup(ptr: *mut Self, target_id: ObjID) -> (usize, [CtxMapItem; MAX_SEC_CTX_MAP_LEN]) {
        unsafe {
            let mut map = *ptr;

            // Vec
            // let x: Vec<CtxMapItem> = map
            //     .map
            //     .into_iter()
            //     .enumerate()
            //     .filter(|(i, item)| {
            //         if *i >= map.len as usize {
            //             return false;
            //         }

            //         item.target_id == target_id
            //     })
            //     .map(|(_, i)| i)
            //     .collect();
            //
            let mut buf = [CtxMapItem {
                target_id: 0.into(),
                item_type: CtxMapItemType::Del,
                len: 0,
                offset: 0,
            }; MAX_SEC_CTX_MAP_LEN as usize];

            let mut len = 0;

            for (i, item) in map.map.into_iter().enumerate() {
                if i > map.len as usize {
                    break;
                }

                if item.target_id == target_id {
                    buf[len] = item;
                    len += 1;
                }
            }

            return (len, buf);
        }
    }

    //TODO:
    // insert
    // remove
    // lookup
}

impl BaseType for SecCtxMap {
    fn fingerprint() -> u64 {
        // lol
        69
    }
}

unsafe impl StoreCopy for SecCtxMap {}

// impl BaseType for SecCtxMap {
//     fn init<T>(_t: T) -> Self {
//         unsafe { *SecCtxMap::parse(_t) }
//     }

//     fn tags() -> &'static [(
//         twizzler_abi::marker::BaseVersion,
//         twizzler_abi::marker::BaseTag,
//     )] {
//         todo!()
//     }
// }
