use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
};

use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    error::ResourceError,
    object::{MapFlags, ObjectHandle},
};

use super::{Allocator, OwnedGlobalPtr, SingleObjectAllocator};
use crate::{
    marker::BaseType,
    object::{Object, ObjectBuilder, RawObject, TxObject},
    ptr::{GlobalPtr, RefMut},
    Result,
};

pub struct ArenaObject {
    obj: Object<ArenaBase>,
}

impl ArenaObject {
    pub fn object(&self) -> &Object<ArenaBase> {
        &self.obj
    }

    pub fn from_allocator(alloc: ArenaAllocator) -> Result<Self> {
        Self::from_objid(alloc.ptr.id())
    }

    pub fn from_objid(id: ObjID) -> Result<Self> {
        Ok(Self {
            obj: Object::map(id, MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST)?,
        })
    }

    pub fn new(builder: ObjectBuilder<ArenaBase>) -> Result<Self> {
        let obj = builder.build(ArenaBase {
            next: (NULLPAGE_SIZE * 2) as u64,
        })?;
        Ok(Self { obj })
    }

    pub fn into_tx(self) -> Result<TxObject<ArenaBase>> {
        self.obj.into_tx()
    }

    pub fn as_tx(&self) -> Result<TxObject<ArenaBase>> {
        self.obj.as_tx()
    }

    pub fn allocator(&self) -> ArenaAllocator {
        ArenaAllocator {
            ptr: GlobalPtr::new(self.obj.id(), NULLPAGE_SIZE as u64),
        }
    }

    pub fn alloc<T>(&self, value: T) -> Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        self.alloc_inplace(|p| Ok(p.write(value)))
    }

    pub fn alloc_inplace<T>(
        &self,
        f: impl FnOnce(RefMut<MaybeUninit<T>>) -> Result<RefMut<T>>,
    ) -> Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        let layout = Layout::new::<T>();
        let alloc = self.allocator().alloc(layout)?.cast::<MaybeUninit<T>>();
        let ptr = unsafe { alloc.resolve().into_mut() };
        let ptr = f(ptr)?;
        Ok(unsafe { OwnedGlobalPtr::from_global(ptr.global().cast(), self.allocator()) })
    }
}

#[derive(Clone, Copy)]
pub struct ArenaAllocator {
    ptr: GlobalPtr<ArenaBase>,
}

impl ArenaAllocator {
    pub fn new(ptr: GlobalPtr<ArenaBase>) -> Self {
        Self { ptr }
    }
}

impl SingleObjectAllocator for ArenaAllocator {}

#[repr(C)]
pub struct ArenaBase {
    next: u64,
}

impl BaseType for ArenaBase {}

impl ArenaBase {
    const MIN_ALIGN: usize = 16;
    fn reserve(&mut self, layout: Layout) -> Result<u64> {
        let align = std::cmp::max(layout.align(), Self::MIN_ALIGN);
        let len = std::cmp::max(layout.size(), Self::MIN_ALIGN) as u64;
        let next_cell = self.next;
        let next = next_cell.next_multiple_of(align as u64);
        if next + len > MAX_SIZE as u64 {
            return Err(ResourceError::OutOfMemory.into());
        }

        self.next = next + len;
        Ok(next)
    }
}

impl Allocator for ArenaAllocator {
    fn alloc(
        &self,
        layout: std::alloc::Layout,
    ) -> core::result::Result<GlobalPtr<u8>, std::alloc::AllocError> {
        // TODO: use try_resolve
        let mut allocator = unsafe { self.ptr.resolve().into_tx() }.map_err(|_| AllocError)?;
        let reserve = allocator.reserve(layout).map_err(|_| AllocError)?;
        let gp = GlobalPtr::new(allocator.handle().id(), reserve);
        Ok(gp)
    }

    unsafe fn dealloc(&self, _ptr: GlobalPtr<u8>, _layout: std::alloc::Layout) {}
}

impl TxObject<ArenaBase> {
    pub fn alloc<T>(&self, value: T) -> Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        self.alloc_inplace(|p| Ok(p.write(value)))
    }

    pub fn alloc_inplace<T>(
        &self,
        f: impl FnOnce(RefMut<MaybeUninit<T>>) -> Result<RefMut<T>>,
    ) -> Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        let layout = Layout::new::<T>();
        let alloc = ArenaAllocator {
            ptr: GlobalPtr::new(self.id(), NULLPAGE_SIZE as u64),
        };
        let allocation = alloc.alloc(layout)?.cast::<MaybeUninit<T>>();
        let mut ptr = unsafe { allocation.resolve().into_tx() }?;
        f(ptr.as_mut())?;
        Ok(unsafe { OwnedGlobalPtr::from_global(ptr.global().cast(), alloc) })
    }
}

impl AsRef<ObjectHandle> for ArenaObject {
    fn as_ref(&self) -> &ObjectHandle {
        self.obj.handle()
    }
}
