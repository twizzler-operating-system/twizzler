use core::fmt::Display;

use log::debug;
use twizzler::{
    marker::BaseType,
    object::{Object, RawObject, TypedObject},
};
use twizzler_abi::object::{ObjID, NULLPAGE_SIZE};

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

const OBJECT_ROOT_OFFSET: usize = size_of::<SecCtxMap>() + NULLPAGE_SIZE;

#[derive(Clone, Copy, Debug, Default)]
pub struct SecCtxMapLookupResult {
    pub len: usize,
    pub items: [CtxMapItem; MAX_SEC_CTX_MAP_LEN],
}

impl SecCtxMap {
    /// inserts a CtxMapItemType into the SecCtxMap and returns the pointer into the object
    /// TODO: there exists an error where you run out of map entries lol
    pub fn insert(sec_obj: &Object<Self>, target_id: ObjID, item_type: CtxMapItemType) -> *mut Cap {
        let mut tx = sec_obj.clone().tx().unwrap();
        let mut base = tx.base_mut();

        //TODO: Find a way to map the write offset into  object so it doesnt overwrite
        // other data
        let mut write_offset = match item_type {
            CtxMapItemType::Cap => base.len as usize + size_of::<Cap>(),
            CtxMapItemType::Del => base.len as usize + size_of::<Del>(),
        } + OBJECT_ROOT_OFFSET;

        debug!("write offset into object for entry: {:#X}", write_offset);

        let alignment = write_offset % 0x10;

        write_offset += 0x10 - alignment;

        let binding = base.len as usize;

        base.buf[binding] = CtxMapItem {
            target_id,
            item_type,
            offset: write_offset as u32,
        };

        base.len += 1;

        let ptr = tx.lea_mut(write_offset, size_of::<Cap>()).expect(
            "Write offset
            should not result in a pointer outside of the object",
        );

        ptr.cast::<Cap>()
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

impl Display for CtxMapItem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Target Id: {:?}\n", self.target_id)?;
        write!(f, "Item Type: {:?}\n", self.item_type)?;
        write!(f, "Offset: {:#X}\n", self.offset)?;
        Ok(())
    }
}
