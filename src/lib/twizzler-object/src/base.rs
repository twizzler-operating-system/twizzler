use crate::{
    marker::{BaseType, BaseVersion, ObjSafe}, object::Object,
};

#[derive(Debug)]
pub enum BaseError {
    InvalidTag,
    InvalidVersion(BaseVersion),
}

impl<T: BaseType + ObjSafe> Object<T> {
    pub fn base_raw(&self) -> Result<&T, BaseError> {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        Ok(unsafe { (start as *const T).as_ref().unwrap() })
    }

    pub fn base_raw_mut(&mut self) -> Result<&mut T, BaseError> {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        Ok(unsafe { (start as *mut T).as_mut().unwrap() })
    }
}

impl<T> Object<T> {
    pub unsafe fn base_raw_unchecked(&self) -> &T {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        (start as *const T).as_ref().unwrap()
    }

    pub unsafe fn base_raw_mut_unchecked(&mut self) -> &mut T {
        let (start, _) = twizzler_abi::slot::to_vaddr_range(self.slot);
        (start as *mut T).as_mut().unwrap()
    }
}
