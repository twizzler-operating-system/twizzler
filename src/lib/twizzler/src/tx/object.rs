use std::{alloc::Layout, mem::MaybeUninit};

use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_rt_abi::object::ObjectHandle;

use crate::{
    marker::BaseType,
    object::{MutObject, Object, RawObject, TypedObject},
    ptr::{GlobalPtr, RefMut},
    Result,
};

#[repr(C)]
pub struct TxObject<T = ()> {
    obj: MutObject<T>,
    static_alloc: usize,
    sync_on_drop: bool,
}

impl<T> TxObject<T> {
    const MIN_ALIGN: usize = 32;
    pub fn new(object: Object<T>) -> Result<Self> {
        // TODO: start tx
        Ok(Self {
            obj: unsafe { object.as_mut()? },
            static_alloc: (size_of::<T>() + align_of::<T>()).next_multiple_of(Self::MIN_ALIGN)
                + NULLPAGE_SIZE,
            sync_on_drop: true,
        })
    }

    pub fn commit(mut self) -> Result<Object<T>> {
        self.sync_on_drop = false;
        self.obj.sync()?;
        let new_obj = unsafe { Object::from_handle_unchecked(self.handle().clone()) };
        Ok(new_obj)
    }

    pub fn abort(mut self) -> Object<T> {
        // TODO: abort tx
        self.sync_on_drop = false;
        unsafe { Object::from_handle_unchecked(self.obj.handle().clone()) }
    }

    pub fn base_mut(&mut self) -> RefMut<'_, T> {
        // TODO: track base in tx
        unsafe { RefMut::from_raw_parts(self.base_mut_ptr(), self.handle()) }
    }

    pub unsafe fn cast<U>(mut self) -> TxObject<U> {
        self.sync_on_drop = false;
        TxObject {
            obj: unsafe { self.obj.clone().cast() },
            static_alloc: self.static_alloc,
            sync_on_drop: self.sync_on_drop,
        }
    }

    pub fn into_unit(self) -> TxObject<()> {
        unsafe { self.cast() }
    }
}

impl<B> TxObject<MaybeUninit<B>> {
    pub fn write(self, baseval: B) -> Result<TxObject<B>> {
        let base = unsafe { self.base_mut_ptr::<MaybeUninit<B>>().as_mut().unwrap() };
        base.write(baseval);
        Ok(unsafe { self.cast() })
    }

    pub fn static_alloc_inplace<T>(
        &mut self,
        f: impl FnOnce(&mut MaybeUninit<T>) -> Result<&mut T>,
    ) -> Result<GlobalPtr<T>> {
        let layout = Layout::new::<T>();
        let start = self.static_alloc.next_multiple_of(layout.align());
        let next_start = (start + layout.size() + layout.align()).next_multiple_of(Self::MIN_ALIGN);
        self.static_alloc = next_start;
        let ptr = unsafe {
            self.obj
                .handle()
                .start()
                .add(start)
                .cast::<MaybeUninit<T>>()
        };
        let mu = unsafe { &mut *ptr };
        f(mu)?;
        let gp = GlobalPtr::new(self.obj.id(), start as u64);
        Ok(gp)
    }

    pub fn static_alloc<T>(&mut self, value: T) -> Result<GlobalPtr<T>> {
        self.static_alloc_inplace(|mu| Ok(mu.write(value)))
    }
}

impl<T> RawObject for TxObject<T> {
    fn handle(&self) -> &twizzler_rt_abi::object::ObjectHandle {
        self.obj.handle()
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

impl<B> Drop for TxObject<B> {
    fn drop(&mut self) {
        if self.sync_on_drop {
            let _ = self
                .obj
                .sync()
                .inspect_err(|e| tracing::error!("TxObject sync on drop failed: {}", e));
        }
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
        self.obj.handle().clone()
    }
}

impl<B> Into<ObjectHandle> for &TxObject<B> {
    fn into(self) -> ObjectHandle {
        self.obj.handle().clone()
    }
}

impl<B> AsRef<ObjectHandle> for TxObject<B> {
    fn as_ref(&self) -> &ObjectHandle {
        self.obj.handle()
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

        let mut tx = obj.into_tx().unwrap();
        let mut base = tx.base_mut();
        base.x = 42;
        drop(base);
        let obj = tx.commit().unwrap();
        assert_eq!(obj.base().x, 42);
    }
}
