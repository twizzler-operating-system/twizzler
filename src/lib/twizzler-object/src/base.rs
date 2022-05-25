use twizzler_abi::meta::MetaInfo;

use crate::{
    marker::{BaseType, BaseVersion, ObjSafe},
    object::Object,
    tx::TxHandle,
};

#[derive(Debug)]
pub enum BaseError {
    InvalidTag,
    InvalidVersion(BaseVersion),
}

fn match_tags<T>(_meta: &MetaInfo) -> Result<(), BaseError> {
    // TODO
    Ok(())
}

impl<T: BaseType + ObjSafe> Object<T> {
    pub fn base(&self, tx: &impl TxHandle) -> Result<&T, BaseError> {
        let meta = unsafe { self.meta_unchecked() };
        match_tags::<T>(meta)?;
        Ok(tx.base(self))
    }

    pub fn base_notx(&self) -> Result<&T, BaseError> {
        let meta = unsafe { self.meta_unchecked() };
        match_tags::<T>(meta)?;
        Ok(unsafe { self.base_unchecked() })
    }
}

impl<T> Object<T> {
    pub unsafe fn base_unchecked(&self) -> &T {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot.slot());
        (start as *const T).as_ref().unwrap()
    }

    pub unsafe fn base_mut_unchecked(&mut self) -> &mut T {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot.slot());
        (start as *mut T).as_mut().unwrap()
    }
}
