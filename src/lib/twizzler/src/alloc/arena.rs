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
    collections::Vector,
    marker::{CopyStorable, Invariant, StorePlace, Storer},
    object::{BaseType, InitializedObject, Object, ObjectBuilder, RawObject},
    ptr::{
        GlobalPtr, InvPtr, InvPtrBuilder, InvSlice, InvSliceBuilder, ResolvedMutPtr, ResolvedPtr,
    },
    tx::{TxCell, TxError, TxHandle, TxResult, UnsafeTxHandle},
};

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

#[derive(twizzler_derive::Invariant, twizzler_derive::BaseType)]
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
    pub fn new() -> Storer<Self> {
        unsafe { Storer::new_move(Self::default()) }
    }

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
        let obj = self.new_object()?;
        // TODO: is this pin unsafe?
        let idx =
            unsafe { self.arenas.as_mut(&tx)?.get_unchecked_mut() }.add_object(obj.base(), &tx)?;
        let ptr = &unsafe { self.arenas.vec.resolve() }[idx];
        let per_object_arena = unsafe { ptr.resolve() };
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
            let slice = unsafe { self.arenas.vec.resolve() };
            let arena = unsafe { slice[start].resolve() };
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

    unsafe fn do_alloc<Item: Invariant>(
        &self,
        init: Item,
    ) -> Result<ResolvedMutPtr<'_, Item>, ArenaError> {
        let layout = Layout::new::<Item>();
        unsafe {
            let gptr = self.alloc_raw(layout)?.cast::<MaybeUninit<Item>>();
            let ptr = gptr
                .resolve()
                .map_err(|_| ArenaError::TransactionFailed(TxError::Exhausted))?;
            let ptr = ptr.into_mut();
            let ptr = ptr.write(init);

            Ok(ptr.owned())
        }
    }

    pub fn alloc<Item: Invariant + CopyStorable>(
        &self,
        init: Item,
    ) -> Result<ResolvedMutPtr<'_, Item>, ArenaError> {
        unsafe { self.do_alloc(init) }
    }

    pub fn alloc_with<Item: Invariant, F, SItem: Into<Storer<Item>>>(
        &self,
        f: F,
    ) -> Result<ResolvedMutPtr<'_, Item>, ArenaError>
    where
        F: FnOnce(StorePlace<'_>) -> SItem,
    {
        let place = unsafe { self.do_alloc::<MaybeUninit<Item>>(MaybeUninit::uninit()) }?;
        let item = f(StorePlace::new(&place.handle()));
        let place = place.write(item.into().into_inner());
        Ok(place)
    }
}

#[derive(twizzler_derive::Invariant, Clone)]
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
        let manifest = unsafe { self.alloc.resolve().map_err(|_| AllocError) }?;
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

#[derive(twizzler_derive::Invariant, twizzler_derive::BaseType)]
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
        let slice = unsafe { self.vec.resolve() };
        self.start += 1;
        let start = self.start as usize;
        if start >= slice.len() {
            return Err(ArenaError::OutOfMemory);
        }
        slice
            .get(start)
            .unwrap()
            .set_with(|in_place| in_place.store(ptr), tx)?;
        Ok(start)
    }
}

#[derive(twizzler_derive::Invariant, twizzler_derive::BaseType)]
#[repr(C)]
pub struct PerObjectArena {
    max: u64,
    end: TxCell<u64>,
}

impl Default for PerObjectArena {
    fn default() -> Self {
        PerObjectArena {
            max: Self::MAX_ALLOC,
            end: TxCell::new((NULLPAGE_SIZE + size_of::<Self>()) as u64),
        }
    }
}

impl PerObjectArena {
    const MAX_ALLOC: u64 = (MAX_SIZE - NULLPAGE_SIZE * 8) as u64;
    fn alloc_raw<'a>(
        &self,
        handle: &ObjectHandle,
        layout: Layout,
        tx: impl TxHandle<'a>,
    ) -> Result<GlobalPtr<u8>, ArenaError> {
        const MIN_ALIGN: usize = 32;
        let align = std::cmp::max(MIN_ALIGN, layout.align());
        let place = self.end.modify(
            |mut end| {
                let place = (*end as usize).next_multiple_of(align);
                let next_end = place + layout.size();

                if next_end > self.max as usize {
                    return Err(ArenaError::OutOfMemory);
                }

                *end = next_end as u64;
                Ok(place)
            },
            tx,
        )??;

        Ok(GlobalPtr::new(handle.id, place as u64))
    }
}

#[cfg(test)]
mod test {
    use super::ArenaManifest;
    use crate::{
        marker::Storer,
        object::{BaseType, InitializedObject, Object, ObjectBuilder},
        ptr::{InvPtr, InvPtrBuilder},
    };

    #[derive(twizzler_derive::Invariant, twizzler_derive::NewStorer)]
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
        let obj = ObjectBuilder::<ArenaManifest>::default()
            .construct(|_| ArenaManifest::new())
            .unwrap();
        let leaf_object = ObjectBuilder::default()
            .init(LeafData { payload: 42 })
            .unwrap();

        let arena = obj.base();
        // Alloc a new node.
        let node1 = arena
            .alloc_with::<Node, _, _>(|mut ip| {
                Node::new_storer(
                    Storer::store(InvPtrBuilder::null(), &mut ip),
                    Storer::store(leaf_object.base(), &mut ip),
                )
            })
            .unwrap();

        // Alloc another node
        let node2 = arena
            .alloc_with::<Node, _, _>(|mut ip| {
                Node::new_storer(
                    Storer::store(node1, &mut ip),
                    Storer::store(leaf_object.base(), &mut ip),
                )
            })
            .unwrap();

        let _leaf_alloc = arena.alloc(LeafData { payload: 32 }).unwrap();

        let res_node1 = unsafe { node2.next.resolve() };
        let leaf_data = unsafe { res_node1.data.resolve() };
        let payload = leaf_data.payload;
        assert_eq!(payload, 42);
    }
}
