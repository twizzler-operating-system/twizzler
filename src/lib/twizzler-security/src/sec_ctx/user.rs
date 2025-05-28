use alloc::collections::btree_map::BTreeMap;
use core::fmt::Display;

use heapless::Vec;
use log::debug;
use twizzler::object::{Object, ObjectBuilder, RawObject, TypedObject};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::ObjectCreate,
};
use twizzler_rt_abi::{
    error::{ResourceError, TwzError},
    object::MapFlags,
};

use super::{CtxMapItem, CtxMapItemType, PermsInfo, SecCtxBase, SecCtxFlags};
use crate::{
    sec_ctx::{MAP_ITEMS_PER_OBJ, OBJECT_ROOT_OFFSET},
    Cap, Del, VerifyingKey,
};

pub struct SecCtx {
    uobj: Object<SecCtxBase>,
    cache: BTreeMap<ObjID, PermsInfo>,
}

impl Default for SecCtx {
    fn default() -> Self {
        let obj = ObjectBuilder::default()
            .build(SecCtxBase::default())
            .unwrap();

        Self {
            uobj: obj,
            cache: BTreeMap::new(),
        }
    }
}

impl Display for SecCtx {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let binding = self.uobj.clone();
        let base = binding.base();

        write!(f, "Sec Ctx ObjID: {} {{\n", self.uobj.id())?;
        write!(f, "base: {:?}", base)?;
        Ok(())
    }
}

impl SecCtx {
    pub fn attached_ctx() -> SecCtx {
        todo!("unsure how to get attached sec_ctx as of rn")
    }

    pub fn new(
        object_create_spec: ObjectCreate,
        global_mask: Protections,
        flags: SecCtxFlags,
    ) -> Result<Self, TwzError> {
        let new_obj =
            ObjectBuilder::new(object_create_spec).build(SecCtxBase::new(global_mask, flags))?;

        Ok(Self {
            uobj: new_obj,
            cache: BTreeMap::new(),
        })
    }

    pub fn insert_cap(&self, cap: Cap) -> Result<(), TwzError> {
        let mut tx = self.uobj.clone().tx()?;
        let mut base = tx.base_mut();

        let mut map_item = {
            base.offset += size_of::<Cap>();

            CtxMapItem {
                item_type: CtxMapItemType::Cap,
                offset: base.offset + OBJECT_ROOT_OFFSET,
            }
        };

        let alignment = 0x10 - (map_item.offset % 0x10);
        map_item.offset += alignment;
        // also have to fix the length in the offset
        base.offset += alignment;

        #[cfg(feature = "log")]
        debug!("write offset into object for entry: {:#X}", map_item.offset);

        // seeing if a vec already exists for target obj, else create new
        if let Some(vec) = base.map.get_mut(&cap.target) {
            vec.push(map_item).map_err(|_e| {
                // only possible error case is it being full
                TwzError::Resource(ResourceError::OutOfResources)
            })?;
        } else {
            let mut new_vec = Vec::<CtxMapItem, MAP_ITEMS_PER_OBJ>::new();
            let _ = new_vec.push(map_item);
            let _ = base.map.insert(cap.target, new_vec).map_err(|e| {
                // only possible error case is it being full
                TwzError::Generic(ResourceError::OutOfResources)
            })?;
        };

        let ptr = tx
            .lea_mut(map_item.offset, size_of::<Cap>())
            .expect("Write offset should not result in a pointer outside of the object")
            .cast::<Cap>();

        // SAFETY: copies the capability into the object, we check that the pointer is valid above
        unsafe {
            *ptr = cap;
        }

        tx.commit()?;

        #[cfg(feature = "log")]
        debug!("Added capability at ptr: {:#?}", ptr);
        Ok(())
    }

    pub fn insert_del(&self, _del: Del) -> Result<(), TwzError> {
        todo!("implement later")
    }

    pub fn id(&self) -> ObjID {
        self.uobj.id()
    }

    pub fn remove_cap(&mut self) {
        todo!("implement later")
    }

    pub fn remove_del(&mut self) {
        todo!("implement later")
    }

    // looks up permission info for requested object
    pub fn lookup(&mut self, target_id: ObjID, v_key: &VerifyingKey) -> PermsInfo {
        // first just check cache
        if let Some(cache_entry) = self.cache.get(&target_id) {
            return *cache_entry;
        };

        let base = self.uobj.base();

        // fetch default protections
        let target_object = Object::map(target_id, MapFlags::READ)
            .expect("target object should exist!")
            .meta_ptr();

        let target_obj_default_prot;

        unsafe {
            let metadata = *target_object;
            default_prot = metadata.default_prot;
        }

        // step 1, add up all the permissions granted by VERIFIED capabilities and delegations
        let mut granted_perms =
            PermsInfo::new(self.id(), target_obj_default_prot, Protections::empty());

        // check for possible items
        let Some(results) = base.map.get(&target_id) else {
            // only default permissions granted, there are no entries in this security context
            // not even worth adding to cache
            return granted_perms;
        };

        for entry in results {
            match entry.item_type {
                CtxMapItemType::Del => {
                    //TODO: skip over for now!! finish up later
                    todo!("Delegations not supported yet for lookup")
                }

                CtxMapItemType::Cap => {
                    // pull capability out of the object

                    let ptr = self
                        .uobj
                        .lea(entry.offset, size_of::<Cap>())
                        .expect("address should be inside of object!")
                        .cast::<Cap>();

                    unsafe {
                        let cap = *ptr;

                        if cap.verify_sig(v_key).is_ok() {
                            granted_perms.provide |= cap.protections;
                        }
                    }
                }
            }
        }

        let Some(mask) = base.masks.get(&target_id) else {
            // no mask inside
            // final perms are granted_perms (intersection) global_mask

            granted_perms.provide &= base.global_mask;

            self.cache.insert(target_id, granted_perms.clone());
            return granted_perms;
        };

        // mask exists, final perms are
        // granted_perms & permmask & (global_mask | override_mask)
        granted_perms.provide =
            granted_perms.provide & mask.permmask & (base.global_mask | mask.ovrmask);

        self.cache.insert(target_id, granted_perms.clone());
        granted_perms
    }
}

impl TryFrom<ObjID> for SecCtx {
    type Error = TwzError;
    fn try_from(value: ObjID) -> Result<Self, Self::Error> {
        let uobj = Object::<SecCtxBase>::map(value, MapFlags::READ | MapFlags::WRITE)?;

        Ok(Self {
            uobj,
            cache: BTreeMap::new(),
        })
    }
}

mod tests {
    use super::*;
    use crate::sec_ctx::SecCtxFlags;

    extern crate test;

    fn test_security_context_creation() {
        let _default_sec_ctx = SecCtx::default();
        let _new_sec_ctx =
            SecCtx::new(Default::default(), Protections::all(), SecCtxFlags::empty())
                .expect("new context should have been created!");
    }
}
