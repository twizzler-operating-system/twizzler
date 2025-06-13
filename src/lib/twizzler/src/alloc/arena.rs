use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::error::ResourceError;

use super::{Allocator, OwnedGlobalPtr, SingleObjectAllocator};
use crate::{
    marker::BaseType,
    object::{Object, ObjectBuilder, RawObject},
    ptr::{GlobalPtr, RefMut},
    tx::{TxCell, TxHandle, TxObject, UnsafeTxHandle},
};

pub struct ArenaObject {
    obj: Object<ArenaBase>,
}

impl ArenaObject {
    pub fn new(builder: ObjectBuilder<ArenaBase>) -> crate::tx::Result<Self> {
        let obj = builder.build(ArenaBase {
            next: TxCell::new((NULLPAGE_SIZE * 2) as u64),
        })?;
        Ok(Self { obj })
    }

    pub fn tx(self) -> crate::tx::Result<TxObject<ArenaBase>> {
        self.obj.tx()
    }

    pub fn allocator(&self) -> ArenaAllocator {
        ArenaAllocator {
            ptr: GlobalPtr::new(self.obj.id(), NULLPAGE_SIZE as u64),
        }
    }

    pub fn alloc<T>(&self, value: T) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        self.alloc_inplace(|p| Ok(p.write(value)))
    }

    pub fn alloc_inplace<T>(
        &self,
        f: impl FnOnce(RefMut<MaybeUninit<T>>) -> crate::tx::Result<RefMut<T>>,
    ) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        let layout = Layout::new::<T>();
        twizzler_abi::klog_println!("A");
        let alloc = self.allocator().alloc(layout)?.cast::<MaybeUninit<T>>();
        twizzler_abi::klog_println!("B");
        let ptr = unsafe { alloc.resolve().mutable() };
        twizzler_abi::klog_println!("C");
        let ptr = f(ptr)?;
        twizzler_abi::klog_println!("D");
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
    next: TxCell<u64>,
}

impl BaseType for ArenaBase {}

impl ArenaBase {
    const MIN_ALIGN: usize = 16;
    fn reserve(&self, layout: Layout, tx: &impl TxHandle) -> crate::tx::Result<u64> {
        let align = std::cmp::max(layout.align(), Self::MIN_ALIGN);
        let len = std::cmp::max(layout.size(), Self::MIN_ALIGN) as u64;
        let next_cell = self.next.get_mut(tx)?;
        let next = next_cell.next_multiple_of(align as u64);
        if next + len > MAX_SIZE as u64 {
            return Err(ResourceError::OutOfMemory.into());
        }

        *next_cell = next + len;
        Ok(next)
    }
}

impl Allocator for ArenaAllocator {
    fn alloc(&self, layout: std::alloc::Layout) -> Result<GlobalPtr<u8>, std::alloc::AllocError> {
        // TODO: use try_resolve
        let allocator = unsafe { self.ptr.resolve_mut() };
        let reserve = allocator
            .reserve(layout, &unsafe { UnsafeTxHandle::new() })
            .map_err(|_| AllocError)?;
        let gp = GlobalPtr::new(allocator.handle().id(), reserve);
        Ok(gp)
    }

    unsafe fn dealloc(&self, _ptr: GlobalPtr<u8>, _layout: std::alloc::Layout) {}
}

impl TxObject<ArenaBase> {
    pub fn alloc<T>(&self, value: T) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        self.alloc_inplace(|p| Ok(p.write(value)))
    }

    pub fn alloc_inplace<T>(
        &self,
        f: impl FnOnce(RefMut<MaybeUninit<T>>) -> crate::tx::Result<RefMut<T>>,
    ) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        let layout = Layout::new::<T>();
        let alloc = ArenaAllocator {
            ptr: GlobalPtr::new(self.id(), NULLPAGE_SIZE as u64),
        };
        let allocation = alloc.alloc(layout)?.cast::<MaybeUninit<T>>();
        let ptr = unsafe { allocation.resolve().mutable() };
        let ptr = f(ptr)?;
        Ok(unsafe { OwnedGlobalPtr::from_global(ptr.global().cast(), alloc) })
    }
}
