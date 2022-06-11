use std::ptr::NonNull;

use twizzler_abi::meta::MetaInfo;

use crate::{
    marker::{BaseType, BaseVersion, ObjSafe},
    object::Object,
};

/// Possible errors from getting a reference to an object's base.
#[derive(Debug)]
pub enum BaseError {
    InvalidTag,
    InvalidVersion(BaseVersion),
}

fn match_tags<T>(_meta: NonNull<MetaInfo>) -> Result<(), BaseError> {
    // TODO
    Ok(())
}

impl<T: BaseType + ObjSafe> Object<T> {
    /// Get a reference to the base of an object. Checks to see if the tags and version information
    /// for the BaseType match.
    pub fn base(&self) -> Result<&T, BaseError> {
        let meta = unsafe { self.meta() };
        match_tags::<T>(meta)?;
        Ok(unsafe { self.base_unchecked() })
    }
}

impl<BaseType> Object<BaseType> {
    /// Get a reference to the base of an object, bypassing version and tag checks.
    ///
    /// # Safety
    /// The caller must ensure that the base of the object really is of type BaseType.
    pub unsafe fn base_unchecked(&self) -> &BaseType {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot.slot());
        (start as *const BaseType).as_ref().unwrap()
    }

    /// Get a mutable reference to the base of an object, bypassing version and tag checks.
    ///
    /// # Safety
    /// The caller must ensure that the base of the object really is of type BaseType.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn base_mut_unchecked(&self) -> &mut BaseType {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot.slot());
        (start as *mut BaseType).as_mut().unwrap()
    }
}
