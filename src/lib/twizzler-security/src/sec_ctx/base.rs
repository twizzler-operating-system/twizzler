use core::fmt::Display;

use heapless::{FnvIndexMap, Vec};
use log::debug;
use twizzler::{
    marker::BaseType,
    object::{Object, RawObject},
};
use twizzler_abi::object::{ObjID, Protections, NULLPAGE_SIZE};
use twizzler_rt_abi::error::TwzError;

use crate::{Cap, Del};

/// completely arbitrary amount of mask entries in a security context
const MASKS_MAX: usize = 16;
const SEC_CTX_MAP_LEN: usize = 16;
const MAP_ITEMS_PER_OBJ: usize = 16;

#[derive(Debug)]
struct Mask {
    /// object whose permissions will be masked.
    target: ObjID,
    /// Specifies a mask on the permissions granted by capabilties and delegations in this security
    /// context.
    mask: Protections,
    /// an override mask on the context's global mask.
    ovrmask: Protections,
    // not sure what these flags are for?
    // flags: BitField
}

impl Mask {
    fn new(target: ObjID, mask: Protections, ovrmask: Protections) -> Self {
        Mask {
            target,
            mask,
            ovrmask,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum CtxMapItemType {
    #[default]
    Cap,
    Del,
}

// holds the masks
// ideally should be a `HashMap` but we dont have those yet so this will have to do...

#[derive(Clone, Copy, Debug, Default)]
pub struct CtxMapItem {
    /// Type of the Map Item
    item_type: CtxMapItemType,
    /// The offset into the object
    offset: usize,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct SecCtxFlags: u16 {
        // a security context should have an undetachable bit,
        const UNDETACHABLE = 1;
    }
}

/// The base of a Security Context, holding a map to the capabilities and delegations stored inside,
/// masks on targets
#[derive(Debug)]
pub struct SecCtxBase {
    // a single object can have multiple Capabilities or Delegations
    map: FnvIndexMap<ObjID, Vec<CtxMapItem, MAP_ITEMS_PER_OBJ>, SEC_CTX_MAP_LEN>,
    masks: FnvIndexMap<ObjID, Mask, MASKS_MAX>,
    global_mask: Protections,
    // the running offset into the object where a new entry can be inserted
    offset: usize,
    // possible flags specific to this security context
    flags: SecCtxFlags,
}

const OBJECT_ROOT_OFFSET: usize = size_of::<SecCtxBase>() + NULLPAGE_SIZE;

pub enum InsertType {
    Cap(Cap),
    Del(Del),
}

impl SecCtxBase {
    fn new(global_mask: Protections, flags: SecCtxFlags) -> Self {
        Self {
            map: FnvIndexMap::<ObjID, Vec<CtxMapItem, MAP_ITEMS_PER_OBJ>, SEC_CTX_MAP_LEN>::new(),
            masks: FnvIndexMap::<ObjID, Mask, MASKS_MAX>::new(),
            global_mask,
            offset: 0,
            flags,
        }
    }

    /// inserts the specified capability or delegation into the object
    pub fn insert(
        sec_obj: &Object<Self>,
        target_id: ObjID,
        insert_type: InsertType,
    ) -> Result<(), TwzError> {
        let mut tx = sec_obj.clone().tx().unwrap();
        let mut base = tx.base_mut();

        // construct the map item with the proper offset into the object
        let mut map_item = match insert_type {
            InsertType::Cap(_) => {
                base.offset += size_of::<Cap>();

                CtxMapItem {
                    item_type: CtxMapItemType::Cap,
                    offset: base.offset + OBJECT_ROOT_OFFSET,
                }
            }
            InsertType::Del(_) => {
                base.offset += size_of::<Del>();
                CtxMapItem {
                    item_type: CtxMapItemType::Del,
                    offset: base.offset + OBJECT_ROOT_OFFSET,
                }
            }
        };

        // fix alignment of pointer
        let alignment = 0x10 - (map_item.offset % 0x10);
        map_item.offset += alignment;
        // also have to fix the length in the offset
        base.offset += alignment;

        debug!("write offset into object for entry: {:#X}", map_item.offset);

        // push new entry into map
        if let Some(vec) = base.map.get_mut(&target_id) {
            vec.push(map_item);
        } else {
            let mut new_vec = Vec::<CtxMapItem, MAP_ITEMS_PER_OBJ>::new();
            new_vec.push(map_item);
            base.map.insert(target_id, new_vec);
        };

        // place entry into the object
        match insert_type {
            InsertType::Cap(cap) => {
                let ptr = tx
                    .lea_mut(map_item.offset, size_of::<Cap>())
                    .expect("Write offset should not result in a pointer outside of the object")
                    .cast::<Cap>();

                unsafe {
                    *ptr = cap;
                }

                tx.commit()?;
                debug!("Added capability at ptr: {:#?}", ptr);
            }
            InsertType::Del(del) => {
                let ptr = tx
                    .lea_mut(map_item.offset, size_of::<Del>())
                    .expect("Write offset should not result in a pointer outside of the object")
                    .cast::<Del>();

                unsafe {
                    *ptr = del;
                }

                tx.commit()?;
                debug!("Added delegation at ptr: {:#?}", ptr);
            }
        }

        Ok(())
    }

    pub fn lookup(
        sec_obj: &Object<Self>,
        target_id: ObjID,
    ) -> Option<Vec<CtxMapItem, MAP_ITEMS_PER_OBJ>> {
        // assuming this tx objects waits on a lock? therefore preventing race conditions, check w
        // daniel
        let mut tx = sec_obj.clone().tx().unwrap();
        let mut base = tx.base_mut();

        let results = base.map.get(&target_id).map(|v| v.clone());
        tx.commit();

        results
    }

    pub fn remove() {
        todo!()
    }
}
impl BaseType for SecCtxBase {
    //NOTE: unsure if this is what the fingerprint "should" be, just chose a random number.
    fn fingerprint() -> u64 {
        16
    }
}

impl Display for CtxMapItem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Item Type: {:?}\n", self.item_type)?;
        write!(f, "Offset: {:#X}\n", self.offset)?;
        Ok(())
    }
}

impl Default for SecCtxBase {
    fn default() -> Self {
        Self::new(Protections::all(), SecCtxFlags::empty())
    }
}
