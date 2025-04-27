use alloc::rc::Rc;
use core::{array, default};

use log::debug;
use twizzler::{
    marker::{BaseType, StoreCopy},
    object::{Object, RawObject, TypedObject},
    tx::TxObject,
};
use twizzler_abi::object::{ObjID, NULLPAGE_SIZE};
use twizzler_rt_abi::object::MapFlags;

use crate::{Cap, Del};

const MAX_SEC_CTX_MAP_LEN: usize = 5;

#[derive(Clone, Copy, Debug, Default)]
/// The *header* of a Security Context Object, holding
/// metadata about the security primitives
/// {Capabilities, Delegations} inside the object
pub struct SecCtxMap {
    /// buffer of map items
    pub buf: [CtxMapItem; MAX_SEC_CTX_MAP_LEN as usize],
    /// internal length to keep track of how full map is
    pub len: u32,
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

const OBJECT_ROOT_OFFSET: usize =
    size_of::<CtxMapItem>() * MAX_SEC_CTX_MAP_LEN + size_of::<u32>() + NULLPAGE_SIZE;

#[derive(Clone, Copy, Debug, Default)]
pub struct SecCtxMapLookupResult {
    pub len: usize,
    pub items: [CtxMapItem; MAX_SEC_CTX_MAP_LEN],
}

impl SecCtxMap {
    /// inserts a CtxMapItemType into the SecCtxMap and returns the write offset into the object
    pub fn insert(sec_obj: &Object<Self>, target_id: ObjID, item_type: CtxMapItemType) -> u32 {
        let mut tx = sec_obj.clone().tx().unwrap();
        let mut base = tx.base_mut();

        //TODO: Find a way to map the write offset into  object so it doesnt overwrite
        // other data
        let mut write_offset = match item_type {
            CtxMapItemType::Cap => base.len + size_of::<Cap>() as u32,
            CtxMapItemType::Del => base.len + size_of::<Del>() as u32,
        } + OBJECT_ROOT_OFFSET as u32;

        debug!("write_offset before adjustment: {:#02x}", write_offset);
        let alignment = write_offset % 0x10;
        debug!("alginment:{:#02x}", alignment);

        write_offset += (0x10 - alignment);
        debug!("write_offset after adjustment: {:#02x}", write_offset);

        // to appease the compiler
        let len = base.len;

        base.buf[len as usize] = CtxMapItem {
            target_id,
            item_type,
            offset: write_offset,
        };

        base.len += 1;

        drop(base);

        tx.commit().unwrap();

        write_offset
    }

    /// Looks up whether or not there exists a map entry for the given target object
    /// inside of the sec_obj
    pub fn lookup(sec_obj: &Object<Self>, target_id: ObjID) -> SecCtxMapLookupResult {
        let mut buf = [CtxMapItem::default(); MAX_SEC_CTX_MAP_LEN];
        let mut len = 0;

        let base = sec_obj.base();

        for (i, item) in base.clone().buf.into_iter().enumerate() {
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

    // lowkey dont know what the semantics for removal are
    pub fn remove() {
        todo!()
    }
}

impl BaseType for SecCtxMap {
    fn fingerprint() -> u64 {
        // lol
        69
    }
}
