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
    ptr::{GlobalPtr, RefMut, RefSliceMut},
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
        let gp = self
            .allocator()
            .alloc_with(|x| f(x).map_err(|_| AllocError))?;
        Ok(unsafe { OwnedGlobalPtr::from_global(gp.cast(), self.allocator()) })
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

    fn alloc_with<T>(
        &self,
        f: impl FnOnce(
            RefMut<MaybeUninit<T>>,
        ) -> core::result::Result<RefMut<T>, std::alloc::AllocError>,
    ) -> core::result::Result<GlobalPtr<u8>, AllocError> {
        let mut allocator = unsafe { self.ptr.resolve().into_tx() }.map_err(|_| AllocError)?;
        let reserve = allocator
            .reserve(Layout::new::<T>())
            .map_err(|_| AllocError)?;
        let gp = GlobalPtr::<u8>::new(allocator.handle().id(), reserve);
        let res = gp.cast::<MaybeUninit<T>>();
        let res = unsafe { res.resolve_mut() };
        Ok(f(res)?.global().cast())
    }

    unsafe fn dealloc(&self, _ptr: GlobalPtr<u8>, _layout: std::alloc::Layout) {}
}

impl TxObject<ArenaBase> {
    pub fn alloc<T>(&mut self, value: T) -> Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        self.alloc_inplace(|p| Ok(p.write(value)))
    }

    pub fn alloc_with_slice<T: Copy>(
        &mut self,
        slice: &[T],
    ) -> Result<(usize, OwnedGlobalPtr<T, ArenaAllocator>)> {
        let layout = Layout::array::<T>(slice.len()).unwrap();
        let reserve = self.base_mut().reserve(layout)?;
        let gp = GlobalPtr::<T>::new(self.id(), reserve);
        let res = unsafe { gp.resolve_mut() };
        let mut slice_alloc = unsafe { RefSliceMut::from_ref(res, slice.len()) };
        slice_alloc.copy_from_slice(slice);
        Ok(unsafe {
            (
                slice.len(),
                OwnedGlobalPtr::from_global(gp.cast(), self.allocator()),
            )
        })
    }

    pub fn alloc_inplace<T>(
        &mut self,
        f: impl FnOnce(RefMut<MaybeUninit<T>>) -> Result<RefMut<T>>,
    ) -> Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        let reserve = self
            .base_mut()
            .reserve(Layout::new::<T>())
            .map_err(|_| AllocError)?;
        let gp = GlobalPtr::<u8>::new(self.id(), reserve);
        let res = gp.cast::<MaybeUninit<T>>();
        let res = unsafe { res.resolve_mut() };
        let gp = f(res)?.global();
        Ok(unsafe { OwnedGlobalPtr::from_global(gp.cast(), self.allocator()) })
    }

    pub fn allocator(&self) -> ArenaAllocator {
        ArenaAllocator {
            ptr: GlobalPtr::new(self.id(), NULLPAGE_SIZE as u64),
        }
    }
}

impl AsRef<ObjectHandle> for ArenaObject {
    fn as_ref(&self) -> &ObjectHandle {
        self.obj.handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectBuilder;

    #[test]
    fn test_arena_object_new() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");

        // Verify the object was created successfully
        assert!(arena.object().handle().id() != ObjID::new(0));
    }

    #[test]
    fn test_arena_allocator() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");
        let allocator = arena.allocator();

        // Test basic allocation
        let layout = Layout::new::<u64>();
        let ptr = allocator.alloc(layout).expect("Failed to allocate");

        // Verify the pointer is valid
        assert!(ptr.offset() >= NULLPAGE_SIZE as u64 * 2);
    }

    #[test]
    fn test_arena_alloc_value() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");

        let value = 42u64;
        let owned_ptr = arena.alloc(value).expect("Failed to allocate value");

        // Verify the allocated value
        let resolved = { owned_ptr.resolve() };
        assert_eq!(*resolved, 42u64);
    }

    #[test]
    fn test_arena_alloc_inplace() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");

        let owned_ptr = arena
            .alloc_inplace(|uninit| Ok(uninit.write(100u32)))
            .expect("Failed to allocate in place");

        // Verify the allocated value
        let resolved = { owned_ptr.resolve() };
        assert_eq!(*resolved, 100u32);
    }

    #[test]
    fn test_arena_multiple_allocations() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");

        // Allocate multiple values
        let ptr1 = arena.alloc(1u64).expect("Failed to allocate first value");
        let ptr2 = arena.alloc(2u64).expect("Failed to allocate second value");
        let ptr3 = arena.alloc(3u64).expect("Failed to allocate third value");

        // Verify all values are correct and pointers are different
        let val1 = { ptr1.resolve() };
        let val2 = { ptr2.resolve() };
        let val3 = { ptr3.resolve() };

        assert_eq!(*val1, 1u64);
        assert_eq!(*val2, 2u64);
        assert_eq!(*val3, 3u64);

        // Verify pointers are at different offsets
        assert_ne!(ptr1.offset(), ptr2.offset());
        assert_ne!(ptr2.offset(), ptr3.offset());
        assert_ne!(ptr1.offset(), ptr3.offset());
    }

    #[test]
    fn test_arena_tx_object() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");

        let mut tx_obj = arena.as_tx().expect("Failed to create tx object");
        let owned_ptr = tx_obj.alloc(999u64).expect("Failed to allocate in tx");

        // Verify the allocated value
        let resolved = { owned_ptr.resolve() };
        assert_eq!(*resolved, 999u64);
    }

    #[test]
    fn test_arena_alignment() {
        let builder = ObjectBuilder::default();
        let arena = ArenaObject::new(builder).expect("Failed to create ArenaObject");

        // Allocate values with different alignments
        let ptr1 = arena.alloc(1u8).expect("Failed to allocate u8");
        let ptr2 = arena.alloc(2u64).expect("Failed to allocate u64");

        // Verify alignment requirements are met
        assert_eq!(ptr1.offset() % ArenaBase::MIN_ALIGN as u64, 0);
        assert_eq!(ptr2.offset() % ArenaBase::MIN_ALIGN as u64, 0);
    }

    #[test]
    fn test_arena_from_objid() {
        let builder = ObjectBuilder::default();
        let arena1 = ArenaObject::new(builder).expect("Failed to create ArenaObject");
        let obj_id = arena1.object().id();

        // Create a new ArenaObject from the same object ID
        let arena2 = ArenaObject::from_objid(obj_id).expect("Failed to create from objid");

        // Verify they reference the same object
        assert_eq!(arena1.object().id(), arena2.object().id());
    }
}
