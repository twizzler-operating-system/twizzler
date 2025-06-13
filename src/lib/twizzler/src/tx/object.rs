use std::{
    alloc::Layout, marker::PhantomData, mem::MaybeUninit, ptr::addr_of, sync::atomic::AtomicU64,
};

use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_map_ctrl, MapControlCmd, SyncFlags, SyncInfo},
};
use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

use super::{Result, TxHandle};
use crate::{
    marker::BaseType,
    object::{Object, RawObject, TypedObject},
    ptr::{GlobalPtr, RefMut},
};

#[repr(C)]
pub struct TxObject<T = ()> {
    handle: ObjectHandle,
    static_alloc: usize,
    _pd: PhantomData<*mut T>,
}

impl<T> TxObject<T> {
    const MIN_ALIGN: usize = 32;
    pub fn new(object: Object<T>) -> Result<Self> {
        // TODO: start tx
        Ok(Self {
            handle: object.into_handle(),
            static_alloc: (size_of::<T>() + align_of::<T>()).next_multiple_of(Self::MIN_ALIGN)
                + NULLPAGE_SIZE,
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

    pub fn into_unit(self) -> TxObject<()> {
        TxObject {
            handle: self.handle,
            static_alloc: self.static_alloc,
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

    pub fn static_alloc_inplace<T>(
        &mut self,
        f: impl FnOnce(&mut MaybeUninit<T>) -> crate::tx::Result<&mut T>,
    ) -> crate::tx::Result<GlobalPtr<T>> {
        let layout = Layout::new::<T>();
        let start = self.static_alloc.next_multiple_of(layout.align());
        let next_start = (start + layout.size() + layout.align()).next_multiple_of(Self::MIN_ALIGN);
        self.static_alloc = next_start;
        let ptr = unsafe { self.handle.start().add(start).cast::<MaybeUninit<T>>() };
        let mu = unsafe { &mut *ptr };
        f(mu)?;
        let gp = GlobalPtr::new(self.handle.id(), start as u64);
        Ok(gp)
    }

    pub fn static_alloc<T>(&mut self, value: T) -> crate::tx::Result<GlobalPtr<T>> {
        self.static_alloc_inplace(|mu| Ok(mu.write(value)))
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

impl<B> Into<ObjectHandle> for TxObject<B> {
    fn into(self) -> ObjectHandle {
        self.handle
    }
}

impl<B> Into<ObjectHandle> for &TxObject<B> {
    fn into(self) -> ObjectHandle {
        self.handle.clone()
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
