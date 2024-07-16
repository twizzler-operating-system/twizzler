use std::{
    alloc::Layout,
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use twizzler_runtime_api::ObjID;

use super::{Allocator, TxAllocator};
use crate::{
    collections::VectorHeader,
    object::{BaseType, Object},
    ptr::{InvPtr, InvPtrBuilder},
    tx::{TxCell, TxError, TxResult},
};

pub struct ArenaMutRef<'arena, T> {
    ptr: &'arena mut T,
    target_id: ObjID,
}

pub struct ArenaRef<'arena, T> {
    ptr: &'arena T,
    target_id: ObjID,
}

impl<'arena, T> From<&ArenaRef<'arena, T>> for InvPtrBuilder<T> {
    fn from(value: &ArenaRef<'arena, T>) -> Self {
        todo!()
    }
}

impl<'arena, T> Deref for ArenaMutRef<'arena, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.ptr
    }
}

impl<'arena, T> Deref for ArenaRef<'arena, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.ptr
    }
}

impl<'arena, T> DerefMut for ArenaMutRef<'arena, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ptr
    }
}

impl<'a, T> From<ArenaRef<'a, T>> for InvPtrBuilder<T> {
    fn from(value: ArenaRef<'a, T>) -> Self {
        todo!()
    }
}

impl<'a, T> From<ArenaMutRef<'a, T>> for InvPtrBuilder<T> {
    fn from(value: ArenaMutRef<'a, T>) -> Self {
        todo!()
    }
}

pub trait Arena<'arena> {
    fn alloc<Item: ArenaItem>(
        &'arena self,
        init: Item,
        placement: Option<Placement>,
    ) -> Result<ArenaMutRef<'arena, Item>, ArenaError>;

    fn alloc_with<Item: ArenaItem, F>(
        &'arena self,
        f: F,
        placement: Option<Placement>,
    ) -> Result<ArenaMutRef<'arena, Item>, ArenaError>
    where
        F: FnOnce(&Object<PerObjectArena>) -> Item;
}

impl Object<ArenaManifest> {
    fn add_object(&self) -> Result<usize, ArenaError> {
        todo!()
        /*
        let new_id = todo!();
        self.object
            .tx(|mut tx| {
                let base = self.object.base();
                let arenas = base.arenas.as_mut(&mut tx);
                Ok(arenas.add_object(new_id))
            })
            .map_err(|_err: TxError<()>| ArenaError::OutOfMemory)
        */
    }
}

impl<'arena> Arena<'arena> for Object<ArenaManifest> {
    fn alloc<Item: ArenaItem>(
        &'arena self,
        init: Item,
        placement: Option<Placement>,
    ) -> Result<ArenaMutRef<'arena, Item>, ArenaError> {
        todo!()
    }

    fn alloc_with<Item: ArenaItem, F>(
        &'arena self,
        f: F,
        placement: Option<Placement>,
    ) -> Result<ArenaMutRef<'arena, Item>, ArenaError>
    where
        F: FnOnce(&Object<PerObjectArena>) -> Item,
    {
        todo!()
    }
}

impl Allocator for Object<ArenaManifest> {
    fn allocate(
        &self,
        layout: Layout,
    ) -> Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        todo!()
    }

    unsafe fn deallocate(
        &self,
        ptr: crate::ptr::GlobalPtr<u8>,
        layout: Layout,
    ) -> Result<(), std::alloc::AllocError> {
        todo!()
    }
}

impl TxAllocator for Object<ArenaManifest> {
    fn allocate<'a>(
        &self,
        layout: Layout,
        tx: impl crate::tx::TxHandle<'a>,
    ) -> Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        todo!()
    }

    unsafe fn deallocate<'a>(
        &self,
        ptr: crate::ptr::GlobalPtr<u8>,
        layout: Layout,
        tx: impl crate::tx::TxHandle<'a>,
    ) -> Result<(), std::alloc::AllocError> {
        todo!()
    }
}

pub trait ArenaItem {}

impl<T: Send + Sync> ArenaItem for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArenaError {
    OutOfMemory,
    TransactionFailed,
}

impl From<TxError<ArenaError>> for ArenaError {
    fn from(value: TxError<ArenaError>) -> Self {
        match value {
            TxError::Abort(e) => e,
            _ => ArenaError::TransactionFailed,
        }
    }
}

pub enum Placement {
    Group(usize),
}

#[repr(C)]
pub struct ArenaManifest {
    nr_groups: u32,
    arenas: TxCell<VecAndStart>,
}

#[repr(C)]
struct VecAndStart {
    start: u32,
    vec: VectorHeader<InvPtr<PerObjectArena>>,
}

impl VecAndStart {
    fn add_object(&mut self, id: ObjID) -> usize {
        todo!()
    }
}

impl BaseType for ArenaManifest {}

#[repr(C)]
pub struct PerObjectArena {
    max: u64,
    end: TxCell<u64>,
}

impl BaseType for PerObjectArena {}

impl<'arena> Arena<'arena> for Object<PerObjectArena> {
    fn alloc<Item: ArenaItem>(
        &'arena self,
        init: Item,
        placement: Option<Placement>,
    ) -> Result<ArenaMutRef<'arena, Item>, ArenaError> {
        /*
        let addr = self.object.tx(|mut tx| {
            let end = self.object.base().end.read(&mut tx);
            let layout = Layout::new::<Item>();
            let addr = end.next_multiple_of(layout.align() as u64);
            if addr + layout.size() as u64 > end {
                return Err(ArenaError::OutOfMemory);
            }

            self.object
                .base()
                .end
                .write(&mut tx, addr + layout.size() as u64);
            Ok(addr)
        })?;

        */
        unsafe { todo!() }
    }

    fn alloc_with<Item: ArenaItem, F>(
        &'arena self,
        f: F,
        placement: Option<Placement>,
    ) -> Result<ArenaMutRef<'arena, Item>, ArenaError>
    where
        F: FnOnce(&Object<PerObjectArena>) -> Item,
    {
        todo!()
    }
}

mod test {
    use super::{Arena, ArenaManifest};
    use crate::{
        object::{BaseType, InitializedObject, Object},
        ptr::InvPtr,
        tx::TxCell,
    };

    #[repr(C)]
    struct Node {
        next: InvPtr<Node>,
        data: InvPtr<LeafData>,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct LeafData {
        payload: u32,
    }

    impl BaseType for LeafData {}

    fn test(obj: Object<ArenaManifest>, leaf_object: Object<LeafData>) {
        // Alloc a new node.
        let mut node1 = obj
            .alloc(
                Node {
                    next: InvPtr::null(),
                    data: InvPtr::null(),
                },
                None,
            )
            .unwrap();
        // Set that node's data pointer.
        node1.ptr.data.set(leaf_object.base());

        // Alloc another node.
        let mut node2 = obj
            .alloc(
                Node {
                    next: InvPtr::null(),
                    data: InvPtr::null(),
                },
                None,
            )
            .unwrap();
        // Set the node's data pointer.
        node2.ptr.data.set(leaf_object.base());
        // Set the next pointer.
        node2.ptr.next.set(node1);

        let res_node1 = node2.next.resolve().unwrap();
        let leaf_data = res_node1.data.resolve().unwrap();
        let _payload = leaf_data.payload;

        // This interacts with the runtime to do safe reads. This has overhead, as the runtime needs
        // to ensure that the data doesn't change. Details to come, but there can be a fast and slow
        // path for this, and it's hopefully not too bad. But that's the price for safety.
        let res_node1 = node2.next.resolve().unwrap();
        // unsafe allows us to skip the runtime check. We know the nodes are all in an arena
        // allocator, and we're done writing to it, so this is safe.
        let leaf_data = res_node1.data.resolve().unwrap();
        // This read is checked, and could be slow. It does a full copy, too, as it's not actually
        // safe in general to create references to object data (&T relies on no one mutating, and we
        // don't know if another compartment is mutating it).
        let leaf_data_read = *leaf_data;
        let _payload = leaf_data_read.payload;
    }
}
