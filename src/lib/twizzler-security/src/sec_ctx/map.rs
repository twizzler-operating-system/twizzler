use core::array;

use twizzler::{
    marker::{BaseType, StoreCopy},
    object::{Object, RawObject},
    tx::TxObject,
};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

const MAX_SEC_CTX_MAP_LEN: usize = 5;

// #[derive(Clone, Copy, Debug)]
#[derive(Clone, Debug)]
pub struct SecCtxMap {
    map: [CtxMapItem; MAX_SEC_CTX_MAP_LEN as usize],
    len: u32,
}

// #[derive(Clone, Copy, Debug)]
#[derive(Clone, Debug)]
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
        let obj = Object::<SecCtxMap>::map(sec_ctx_id, MapFlags::READ | MapFlags::WRITE).unwrap();
        obj.base_mut_ptr::<SecCtxMap>()
    }

    /// inserts a CtxMapItemType into the SecCtxMap and returns the write offset into the object
    pub fn insert(obj: Object<Self>, target_id: ObjID, item_type: CtxMapItemType, len: u32) -> u32 {
        let mut tx = obj.tx().unwrap();
        let mut base = tx.base_mut();

        //TODO: need to actually calculate this out / worry about allocation strategies
        let write_offset = base.len * len + size_of::<SecCtxMap>() as u32;
        let len = base.len;

        base.map[len as usize] = CtxMapItem {
            target_id,
            item_type,
            len,
            offset: write_offset,
        };
        base.len += 1;

        drop(base);

        tx.commit().unwrap();

        return write_offset;
    }

    pub fn new() -> Self {
        Self {
            map: array::from_fn(|_| CtxMapItem {
                target_id: 0.into(),
                item_type: CtxMapItemType::Del,
                len: 0,
                offset: 0,
            }),
            len: 0,
        }
    }

    /// size && array of items
    pub fn lookup(ptr: *mut Self, target_id: ObjID) -> (usize, [CtxMapItem; MAX_SEC_CTX_MAP_LEN]) {
        unsafe {
            let mut buf = array::from_fn(|_i| CtxMapItem {
                target_id: 0.into(),
                item_type: CtxMapItemType::Del,
                len: 0,
                offset: 0,
            });

            let mut len = 0;

            for (i, item) in (*ptr).clone().map.into_iter().enumerate() {
                if i > (*ptr).len as usize {
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
