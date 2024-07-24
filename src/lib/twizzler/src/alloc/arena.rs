use std::{
    alloc::{AllocError, Layout},
    marker::PhantomData,
    mem::{size_of, MaybeUninit},
    ops::{Deref, DerefMut},
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_runtime_api::{ObjID, ObjectHandle};

use super::{Allocator, TxAllocator};
use crate::{
    collections::VectorHeader,
    marker::{InPlace, Invariant},
    object::{BaseRef, BaseType, InitializedObject, Object, ObjectBuilder, RawObject},
    ptr::{
        GlobalPtr, InvPtr, InvPtrBuilder, InvSlice, InvSliceBuilder, ResolvedMutPtr, ResolvedPtr,
    },
    tx::{TxCell, TxError, TxHandle, TxResult, UnsafeTxHandle},
};

pub struct ArenaMutRef<'a, T> {
    ptr: ResolvedMutPtr<'a, T>,
}

pub struct ArenaRef<'a, T> {
    ptr: ResolvedPtr<'a, T>,
}

impl<'arena, T> Deref for ArenaMutRef<'arena, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &*self.ptr
    }
}

impl<'arena, T> Deref for ArenaRef<'arena, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &*self.ptr
    }
}

impl<'arena, T> DerefMut for ArenaMutRef<'arena, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.ptr
    }
}

impl<'a, T> From<ArenaRef<'a, T>> for InvPtrBuilder<T> {
    fn from(value: ArenaRef<'a, T>) -> Self {
        unsafe { InvPtrBuilder::from_global(GlobalPtr::from_va(value.ptr.ptr()).unwrap()) }
    }
}

impl<'a, T> From<ArenaMutRef<'a, T>> for InvPtrBuilder<T> {
    fn from(value: ArenaMutRef<'a, T>) -> Self {
        unsafe { InvPtrBuilder::from_global(GlobalPtr::from_va(value.ptr.ptr()).unwrap()) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArenaError {
    OutOfMemory,
    TransactionFailed(TxError),
}

impl From<ArenaError> for AllocError {
    fn from(value: ArenaError) -> Self {
        AllocError
    }
}

impl From<TxError> for ArenaError {
    fn from(value: TxError) -> Self {
        Self::TransactionFailed(value)
    }
}

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
pub struct ArenaManifest {
    arenas: TxCell<VecAndStart>,
}

impl Default for ArenaManifest {
    fn default() -> Self {
        Self {
            arenas: TxCell::new(VecAndStart {
                start: 0,
                vec: InvSlice::null(),
            }),
        }
    }
}

impl ArenaManifest {
    fn new_object(&self) -> Result<Object<PerObjectArena>, ArenaError> {
        let obj = ObjectBuilder::default()
            .init(PerObjectArena::default())
            .map_err(|_| ArenaError::OutOfMemory)?;
        Ok(obj)
    }

    fn add_object<'a>(
        &self,
        tx: impl TxHandle<'a>,
    ) -> Result<ResolvedPtr<PerObjectArena>, ArenaError> {
        println!("arena new obj: add_object");
        let obj = self.new_object()?;
        println!("got: {:p}", obj.base_ptr());
        let idx = unsafe { self.arenas.as_mut(&tx)? }.add_object(obj.base(), &tx)?;
        let ptr = &self.arenas.vec.resolve().unwrap()[idx];
        let per_object_arena = ptr.resolve().unwrap();
        Ok(per_object_arena.owned())
    }

    fn alloc_raw(&self, layout: Layout) -> Result<GlobalPtr<u8>, ArenaError> {
        let tx = unsafe { UnsafeTxHandle::new() };
        if self.arenas.vec.is_null() {
            let obj = ObjectBuilder::default()
                .init(())
                .map_err(|_| ArenaError::OutOfMemory)?;

            let raw_base = obj.base_mut_ptr() as *mut TxCell<InvPtr<PerObjectArena>>;
            let raw_len =
                (MAX_SIZE - NULLPAGE_SIZE * 2) / size_of::<TxCell<InvPtr<PerObjectArena>>>();

            let slice = unsafe {
                InvSliceBuilder::from_raw_parts(
                    InvPtrBuilder::from_global(GlobalPtr::from_va(raw_base).unwrap()),
                    raw_len,
                )
            };

            self.arenas.set_with(
                |in_place| VecAndStart {
                    start: 0,
                    vec: in_place.store(slice),
                },
                tx,
            )?;
            let arena = self.add_object(tx)?;
            let handle = arena.handle().clone();
            arena.alloc_raw(&handle, layout, tx)
        } else {
            let start = self.arenas.start as usize;
            let slice = self.arenas.vec.resolve().unwrap();
            let arena = slice[start].resolve().unwrap();
            let handle = arena.handle().clone();
            let raw_place = arena.alloc_raw(&handle, layout, tx);

            if matches!(raw_place, Err(ArenaError::OutOfMemory)) {
                let arena = self.add_object(tx)?;
                let handle = arena.handle().clone();
                arena.alloc_raw(&handle, layout, tx)
            } else {
                raw_place
            }
        }
    }

    pub fn alloc<Item: Invariant>(&self, init: Item) -> Result<ArenaMutRef<'_, Item>, ArenaError> {
        let layout = Layout::new::<Item>();
        unsafe {
            let gptr = self.alloc_raw(layout)?.cast::<Item>();
            let ptr = gptr
                .resolve()
                .map_err(|_| ArenaError::TransactionFailed(TxError::Exhausted))?;
            let ptr = ptr.as_mut();
            ptr.ptr().write(init);

            Ok(ArenaMutRef { ptr: ptr.owned() })
        }
    }

    pub fn alloc_with<Item: Invariant, F>(&self, f: F) -> Result<ArenaMutRef<'_, Item>, ArenaError>
    where
        F: FnOnce(InPlace<'_>) -> Item,
    {
        let place = self.alloc::<MaybeUninit<Item>>(MaybeUninit::uninit())?;
        let item = f(InPlace::new(&place.ptr.handle()));
        let place = place.ptr.write(item);
        Ok(ArenaMutRef { ptr: place })
    }
}

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
pub struct ArenaAllocator {
    alloc: GlobalPtr<ArenaManifest>,
}

impl ArenaAllocator {
    pub fn new(alloc: &ArenaManifest) -> Self {
        Self {
            alloc: GlobalPtr::from_va(alloc).unwrap(),
        }
    }
}

impl Allocator for ArenaAllocator {
    fn allocate(
        &self,
        layout: Layout,
    ) -> Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        let manifest = self.alloc.resolve().map_err(|_| AllocError)?;
        manifest.allocate(layout)
    }

    unsafe fn deallocate(
        &self,
        _ptr: crate::ptr::GlobalPtr<u8>,
        _layout: Layout,
    ) -> Result<(), std::alloc::AllocError> {
        Ok(())
    }
}

impl Allocator for ArenaManifest {
    fn allocate(
        &self,
        layout: Layout,
    ) -> Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        let ptr = self.alloc_raw(layout)?;
        Ok(ptr)
    }

    unsafe fn deallocate(
        &self,
        _ptr: crate::ptr::GlobalPtr<u8>,
        _layout: Layout,
    ) -> Result<(), std::alloc::AllocError> {
        Ok(())
    }
}

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
struct VecAndStart {
    start: u32,
    vec: InvSlice<TxCell<InvPtr<PerObjectArena>>>,
}

impl VecAndStart {
    fn add_object<'a>(
        &mut self,
        ptr: impl Into<InvPtrBuilder<PerObjectArena>>,
        tx: impl TxHandle<'a>,
    ) -> Result<usize, ArenaError> {
        let slice = self.vec.resolve().unwrap();
        self.start += 1;
        let start = self.start as usize;
        if start >= slice.len() {
            return Err(ArenaError::OutOfMemory);
        }
        println!("slice ptr: {:p}", slice.get(start).unwrap().ptr());
        slice
            .get(start)
            .unwrap()
            .set_with(|in_place| in_place.store(ptr), tx)?;
        Ok(start)
    }
}

impl BaseType for ArenaManifest {}

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
pub struct PerObjectArena {
    max: u64,
    end: TxCell<u64>,
}

impl Default for PerObjectArena {
    fn default() -> Self {
        PerObjectArena {
            max: (MAX_SIZE - NULLPAGE_SIZE) as u64,
            end: TxCell::new((NULLPAGE_SIZE + size_of::<Self>()) as u64),
        }
    }
}

impl PerObjectArena {
    fn alloc_raw<'a>(
        &self,
        handle: &ObjectHandle,
        layout: Layout,
        tx: impl TxHandle<'a>,
    ) -> Result<GlobalPtr<u8>, ArenaError> {
        const MIN_ALIGN: usize = 32;
        let align = std::cmp::max(MIN_ALIGN, layout.align());
        let place = self.end.modify(
            |end| {
                let place = (*end as usize).next_multiple_of(align);
                let next_end = place + layout.size();
                *end = next_end as u64;
                place
            },
            tx,
        )?;

        Ok(unsafe { GlobalPtr::new(handle.id, place as u64) })
    }
}

impl BaseType for PerObjectArena {}

//#[cfg(test)]
mod test {
    use super::{ArenaManifest, ArenaMutRef};
    use crate::{
        object::{BaseType, InitializedObject, Object, ObjectBuilder},
        ptr::{InvPtr, InvPtrBuilder},
    };

    #[derive(twizzler_derive::Invariant)]
    #[repr(C)]
    struct Node {
        next: InvPtr<Node>,
        data: InvPtr<LeafData>,
    }

    #[derive(twizzler_derive::Invariant, Copy, Clone, Default)]
    #[repr(C)]
    struct LeafData {
        payload: u32,
    }
    impl BaseType for LeafData {
        /* TODO */
    }

    #[test]
    fn test() {
        let obj = ObjectBuilder::default()
            .init(ArenaManifest::default())
            .unwrap();
        let leaf_object = ObjectBuilder::default()
            .init(LeafData { payload: 42 })
            .unwrap();

        let arena = obj.base();
        let leaf2 = arena.alloc(LeafData { payload: 32 }).unwrap();
        // Alloc a new node.
        let node1 = arena
            .alloc_with(|mut ip| Node {
                next: InvPtr::null(),
                data: ip.store(leaf_object.base()),
            })
            .unwrap();

        // Alloc another node
        let node2 = arena
            .alloc_with(|mut ip| Node {
                // this node points to node1 in the next field.
                next: ip.store(node1),
                data: ip.store(leaf2),
            })
            .unwrap();

        let _leaf_alloc = arena.alloc(LeafData { payload: 32 }).unwrap();

        // I'm planning on implementing Deref for InvPtr, and just having it panic if resolve()
        // returns Err.
        let res_node1 = node2.next.resolve().unwrap();
        let leaf_data = res_node1.data.resolve().unwrap();
        let payload = leaf_data.payload;
        assert_eq!(payload, 42);
    }
}
