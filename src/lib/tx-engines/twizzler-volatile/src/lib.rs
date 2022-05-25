use twizzler_object::{
    ptr::{EffAddr, InvPtr, LeaError},
    slot::Slot,
    tx::TxHandle,
};

pub struct TxHandleVolatile {}

impl TxHandleVolatile {
    pub fn new() -> Self {
        Self {}
    }
}

impl TxHandle for TxHandleVolatile {
    #[inline]
    fn txcell_get<'a, T>(
        &self,
        cell: &'a twizzler_object::cell::TxCell<T>,
    ) -> Result<&'a T, twizzler_object::tx::TxError> {
        Ok(unsafe { cell.get_unchecked() })
    }

    fn txcell_get_mut<'a, T>(
        &self,
        _cell: &'a twizzler_object::cell::TxCell<T>,
    ) -> Result<&'a mut T, twizzler_object::tx::TxError> {
        panic!("TxHandleVolatile does not allow for transactional interior mutability");
    }

    #[inline]
    fn base<'a, T>(&self, obj: &'a twizzler_object::Object<T>) -> &'a T {
        unsafe { obj.base_unchecked() }
    }

    fn ptr_resolve<Target>(
        &self,
        ptr: &InvPtr<Target>,
        slot: &std::sync::Arc<Slot>,
    ) -> Result<EffAddr<Target>, LeaError> {
        let (fote, off) = ptr.parts(self)?;
        if fote == 0 {
            return Ok(EffAddr::new(
                slot.clone(),
                (slot.vaddr_null() + off as usize) as *const Target,
            ));
        }
        let fote = unsafe { slot.get_fote_unchecked(fote) };
        let (id, prot) = fote.resolve(self)?;
        let target_slot = twizzler_object::slot::get(id, prot)?;
        let p = target_slot.raw_lea(off as usize);
        Ok(EffAddr::new(target_slot, p))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
