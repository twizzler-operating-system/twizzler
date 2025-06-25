use std::{marker::PhantomData, ptr::addr_of, sync::atomic::AtomicU64};

use twizzler_abi::{
    object::{ObjID, MAX_SIZE},
    syscall::{sys_map_ctrl, MapControlCmd, SyncFlags, SyncInfo},
};
use twizzler_rt_abi::{
    error::TwzError,
    object::{MapFlags, ObjectHandle},
};

use super::{Object, RawObject, TypedObject};
use crate::{
    marker::BaseType,
    ptr::{Ref, RefMut},
};

pub struct MutObject<Base> {
    handle: ObjectHandle,
    _pd: PhantomData<*mut Base>,
}

unsafe impl<Base> Sync for MutObject<Base> {}
unsafe impl<Base> Send for MutObject<Base> {}

impl<B> Clone for MutObject<B> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            _pd: PhantomData,
        }
    }
}

impl<Base> MutObject<Base> {
    pub unsafe fn from_handle_unchecked(handle: ObjectHandle) -> Self {
        Self {
            handle,
            _pd: PhantomData,
        }
    }

    pub fn from_handle(handle: ObjectHandle) -> Result<Self, TwzError> {
        // TODO: check base fingerprint
        unsafe { Ok(Self::from_handle_unchecked(handle)) }
    }

    pub fn into_handle(self) -> ObjectHandle {
        self.handle
    }

    pub unsafe fn cast<U>(self) -> MutObject<U> {
        MutObject {
            handle: self.handle,
            _pd: PhantomData,
        }
    }

    pub fn map(id: ObjID, flags: MapFlags) -> Result<Self, TwzError> {
        // TODO: check base fingerprint
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        tracing::debug!("map: {} {:?} => {:?}", id, flags, handle.start());
        Self::from_handle(handle)
    }

    pub unsafe fn map_unchecked(id: ObjID, flags: MapFlags) -> Result<Self, TwzError> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        unsafe { Ok(Self::from_handle_unchecked(handle)) }
    }

    pub fn id(&self) -> ObjID {
        self.handle.id()
    }

    pub fn update(&mut self) -> crate::Result<()> {
        sys_map_ctrl(self.handle.start(), MAX_SIZE, MapControlCmd::Update, 0)
    }

    pub fn base_mut(&mut self) -> RefMut<'_, Base> {
        unsafe { RefMut::from_raw_parts(self.base_mut_ptr(), &self.handle) }
    }

    pub fn sync(&mut self) -> Result<(), TwzError> {
        let flags = self.handle.map_flags();
        tracing::trace!("sync on {:?} with flags {:?}", self.id(), flags);
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
                self.handle.start(),
                MAX_SIZE,
                MapControlCmd::Sync(sync_info_ptr),
                0,
            )?;
        }
        Ok(())
    }

    pub fn into_object(self) -> Object<Base> {
        unsafe { Object::from_handle_unchecked(self.into_handle()) }
    }

    pub fn as_object(&self) -> Object<Base> {
        unsafe { Object::from_handle_unchecked(self.handle().clone()) }
    }
}

impl<Base> RawObject for MutObject<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<Base: BaseType> TypedObject for MutObject<Base> {
    type Base = Base;

    fn base_ref(&self) -> Ref<'_, Self::Base> {
        let base = self.base_ptr();
        unsafe { Ref::from_raw_parts(base, self.handle()) }
    }

    #[inline]
    fn base(&self) -> &Self::Base {
        unsafe { self.base_ptr::<Self::Base>().as_ref().unwrap_unchecked() }
    }
}

impl<T> AsRef<ObjectHandle> for MutObject<T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}

impl<B> From<MutObject<B>> for Object<B> {
    fn from(mut_obj: MutObject<B>) -> Self {
        unsafe { Object::from_handle_unchecked(mut_obj.into_handle()) }
    }
}
