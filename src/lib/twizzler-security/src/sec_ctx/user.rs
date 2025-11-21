use core::fmt::Display;

use heapless::Vec;
use twizzler::object::{Object, ObjectBuilder, RawObject, TypedObject};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        sys_sctx_attach, sys_thread_active_sctx_id, sys_thread_set_active_sctx_id, ObjectCreate,
    },
};
use twizzler_rt_abi::{
    error::{ResourceError, TwzError},
    object::MapFlags,
};

use super::{CtxMapItem, CtxMapItemType, SecCtxBase, SecCtxFlags};
use crate::{
    sec_ctx::{MAP_ITEMS_PER_OBJ, OBJECT_ROOT_OFFSET},
    Cap, Del,
};

#[derive(Debug)]
/// A User-space representation of a Security Context.
pub struct SecCtx {
    uobj: Object<SecCtxBase>,
}

impl Default for SecCtx {
    fn default() -> Self {
        let obj = ObjectBuilder::default()
            .build(SecCtxBase::default())
            .unwrap();

        Self { uobj: obj }
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
    /// Returns the currently active `SecCtx`.
    pub fn active_ctx() -> SecCtx {
        let curr_sec_ctx_id = sys_thread_active_sctx_id();

        Self::try_from(curr_sec_ctx_id)
            .expect("We should always be able to parse the currently attached security context")
    }

    /// Attaches the current process to this `SecCtx`.
    pub fn attach(&self) -> Result<(), TwzError> {
        sys_sctx_attach(self.id())
    }

    /// Sets this `SecCtx` as the active Security Context.
    pub fn set_active(&self) -> Result<(), TwzError> {
        sys_sctx_attach(self.id())?;
        sys_thread_set_active_sctx_id(self.id())
    }

    /// Returns the `SecCtxBase` of this `SecCtx`.
    pub fn base(&self) -> &SecCtxBase {
        self.uobj.base()
    }

    /// Create a new `SecCtx`.
    pub fn new(
        object_create_spec: ObjectCreate,
        global_mask: Protections,
        flags: SecCtxFlags,
    ) -> Result<Self, TwzError> {
        let new_obj =
            ObjectBuilder::new(object_create_spec).build(SecCtxBase::new(global_mask, flags))?;

        Ok(Self { uobj: new_obj })
    }

    /// Insert a `Cap` into this `SecCtx`.
    pub fn insert_cap(&mut self, cap: Cap) -> Result<(), TwzError> {
        let mut tx = self.uobj.clone().into_tx()?;
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
        log::debug!("write offset into object for entry: {:#X}", map_item.offset);

        // seeing if a vec already exists for target obj, else create new
        if let Some(vec) = base.map.get_mut(&cap.target) {
            vec.push(map_item).map_err(|_| {
                // only possible error case is it being full
                TwzError::Resource(ResourceError::OutOfResources)
            })?;
        } else {
            let mut new_vec = Vec::<CtxMapItem, MAP_ITEMS_PER_OBJ>::new();
            let _ = new_vec.push(map_item);
            let _ = base.map.insert(cap.target, new_vec).map_err(|_| {
                // only possible error case is it being full
                TwzError::Resource(ResourceError::OutOfResources)
            })?;
        };

        let ptr = tx
            .lea_mut(map_item.offset, size_of::<Cap>())
            .expect("Write offset should not result in a pointer outside of the object")
            .cast::<Cap>();

        // SAFETY: copies the capability into the object, we check that the pointer is valid above /
        // fix its alignment
        unsafe {
            *ptr = cap;
        }

        tx.commit()?;

        #[cfg(feature = "log")]
        log::debug!("Added capability at ptr: {:#?}", ptr);
        Ok(())
    }

    /// Insert a `Del` into this `SecCtx`.
    ///
    /// # Panics
    /// is not implemented yet
    pub fn insert_del(&mut self, _del: Del) -> Result<(), TwzError> {
        unimplemented!()
    }

    /// Returns the `ObjID` of this `SecCtx`.
    pub fn id(&self) -> ObjID {
        self.uobj.id()
    }

    /// Remove a `Cap` from this `SecCtx`.
    ///
    /// # Panics
    /// is not implemented yet
    pub fn remove_cap(&mut self) {
        unimplemented!()
    }

    /// Remove a `Del` from this `SecCtx`.
    ///
    /// # Panics
    /// is not implemented yet
    pub fn remove_del(&mut self) {
        unimplemented!()
    }
}

impl TryFrom<ObjID> for SecCtx {
    type Error = TwzError;
    fn try_from(value: ObjID) -> Result<Self, Self::Error> {
        let uobj = Object::<SecCtxBase>::map(value, MapFlags::READ | MapFlags::WRITE)?;

        Ok(Self { uobj })
    }
}
#[cfg(test)]
#[cfg(feature = "user")]
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
