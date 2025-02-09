use alloc::rc::Rc;
use core::{array, default};

use twizzler::{
    marker::{BaseType, StoreCopy},
    object::{Object, RawObject, TypedObject},
    tx::TxObject,
};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

use crate::{Cap, Del};

const MAX_SEC_CTX_MAP_LEN: usize = 5;

#[derive(Clone, Copy, Debug, Default)]
/// The *header* of a Security Context Object, holding
/// metadata about the security primitives
/// {Capabilities, Delegations} inside the object
pub struct SecCtxMap {
    /// buffer of map items
    map: [CtxMapItem; MAX_SEC_CTX_MAP_LEN as usize],
    /// internal length to keep track of how full map is
    len: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CtxMapItem {
    /// The target object id
    target_id: ObjID,
    /// Type of the Map Item
    item_type: CtxMapItemType,
    /// The offset into the object
    offset: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum CtxMapItemType {
    #[default]
    Cap,
    Del,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SecCtxMapLookupResult {
    pub len: usize,
    pub items: [CtxMapItem; MAX_SEC_CTX_MAP_LEN],
}

impl SecCtxMap {
    /// inserts a CtxMapItemType into the SecCtxMap and returns the write offset into the object
    pub fn insert(obj: &Object<Self>, target_id: ObjID, item_type: CtxMapItemType) -> u32 {
        let mut tx = obj.clone().tx().unwrap();
        let mut base = tx.base_mut();

        //TODO: Find a way to map the write offset into an object so it doesnt overwrite
        // other data
        let write_offset = match item_type {
            CtxMapItemType::Cap => base.len + size_of::<Cap>() as u32,
            CtxMapItemType::Del => base.len + size_of::<Del>() as u32,
        };

        // to appease the compiler
        let len = base.len;

        base.map[len as usize] = CtxMapItem {
            target_id,
            item_type,
            offset: write_offset,
        };

        base.len += 1;

        drop(base);

        tx.commit().unwrap();

        write_offset
    }

    pub fn new() -> Self {
        Self {
            map: [CtxMapItem::default(); MAX_SEC_CTX_MAP_LEN],
            len: 0,
        }
    }

    pub fn lookup(obj: &Object<Self>, target_id: ObjID) -> SecCtxMapLookupResult {
        let mut buf = [CtxMapItem::default(); MAX_SEC_CTX_MAP_LEN];
        let mut len = 0;

        let base = obj.base();

        for (i, item) in base.clone().map.into_iter().enumerate() {
            if i > base.len as usize {
                break;
            }

            if item.target_id == target_id {
                buf[len] = item;
                len += 1;
            }
        }

        SecCtxMapLookupResult { len, items: buf }
    }

    //TODO:
    // remove
}

impl BaseType for SecCtxMap {
    fn fingerprint() -> u64 {
        // lol
        69
    }
}

// unsafe impl StoreCopy for SecCtxMap {}
