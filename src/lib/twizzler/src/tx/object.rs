use std::{marker::PhantomData, mem::MaybeUninit, ptr::addr_of, sync::atomic::AtomicU64};

use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{sys_map_ctrl, MapControlCmd, SyncFlags, SyncInfo},
};
use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

use super::{Result, TxHandle};
use crate::{
    marker::BaseType,
    object::{FotEntry, Object, RawObject, TypedObject},
    ptr::RefMut,
};

#[repr(C)]
pub struct TxObject<T = ()> {
    handle: ObjectHandle,
    _pd: PhantomData<*mut T>,
}

impl<T> TxObject<T> {
    pub fn new(object: Object<T>) -> Result<Self> {
        // TODO: start tx
        Ok(Self {
            handle: object.into_handle(),
            _pd: PhantomData,
        })
    }

    pub fn commit(self) -> Result<Object<T>> {
        let handle = self.handle;
        let flags = handle.map_flags();
        if flags.contains(MapFlags::PERSIST) {
            let release = AtomicU64::new(0);
            let release_ptr = addr_of!(release);
            let sync_info = SyncInfo {
                release: release_ptr,
                release_compare: 0,
                release_set: 1,
                durable: core::ptr::null(),
                flags: SyncFlags::DURABLE | SyncFlags::ASYNC_DURABLE,
            };
            let sync_info_ptr = addr_of!(sync_info);
            sys_map_ctrl(
                handle.start(),
                MAX_SIZE,
                MapControlCmd::Sync(sync_info_ptr),
                0,
            )?;
        }
        let new_obj = unsafe { Object::map_unchecked(handle.id(), flags) }?;
        // TODO: commit tx
        Ok(new_obj)
    }

    pub fn abort(self) -> Object<T> {
        // TODO: abort tx
        unsafe { Object::from_handle_unchecked(self.handle) }
    }

    pub fn base_mut(&mut self) -> RefMut<'_, T> {
        // TODO: track base in tx
        unsafe { RefMut::from_raw_parts(self.base_mut_ptr(), self.handle()) }
    }

    pub fn insert_fot(&self, fot: &FotEntry) -> crate::tx::Result<u32> {
        twizzler_rt_abi::object::twz_rt_insert_fot(self.handle(), (fot as *const FotEntry).cast())
    }

    pub fn into_unit(self) -> TxObject<()> {
        TxObject {
            handle: self.handle,
            _pd: PhantomData,
        }
    }
}

impl<B> TxObject<MaybeUninit<B>> {
    pub fn write(self, baseval: B) -> crate::tx::Result<TxObject<B>> {
        let base = unsafe { self.base_mut_ptr::<MaybeUninit<B>>().as_mut().unwrap() };
        base.write(baseval);
        TxObject::new(unsafe { Object::from_handle_unchecked(self.handle) })
    }
}

impl<B> TxHandle for TxObject<B> {
    fn tx_mut(&self, data: *const u8, _len: usize) -> super::Result<*mut u8> {
        // TODO
        Ok(data as *mut u8)
    }
}

impl<T> RawObject for TxObject<T> {
    fn handle(&self) -> &twizzler_rt_abi::object::ObjectHandle {
        &self.handle
    }
}

impl<B: BaseType> TypedObject for TxObject<B> {
    type Base = B;

    fn base_ref(&self) -> crate::ptr::Ref<'_, Self::Base> {
        unsafe { crate::ptr::Ref::from_raw_parts(self.base_ptr(), self.handle()) }
    }

    fn base(&self) -> &Self::Base {
        unsafe { self.base_ptr::<Self::Base>().as_ref().unwrap_unchecked() }
    }
}

impl<B> AsRef<TxObject<()>> for TxObject<B> {
    fn as_ref(&self) -> &TxObject<()> {
        let this = self as *const Self;
        // Safety: This phantom data is the only generic field, and we are repr(C).
        unsafe { this.cast::<TxObject<()>>().as_ref().unwrap() }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        marker::BaseType,
        object::{ObjectBuilder, TypedObject},
    };

    struct Simple {
        x: u32,
    }

    impl BaseType for Simple {}

    #[test]
    fn single_tx() {
        let builder = ObjectBuilder::default();
        let obj = builder.build(Simple { x: 3 }).unwrap();
        let base = obj.base_ref();
        assert_eq!(base.x, 3);
        drop(base);

        let mut tx = obj.tx().unwrap();
        let mut base = tx.base_mut();
        base.x = 42;
        drop(base);
        let obj = tx.commit().unwrap();
        assert_eq!(obj.base().x, 42);
    }
}
