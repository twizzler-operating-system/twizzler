use std::mem::MaybeUninit;

use crate::{
    alloc::{invbox::InvBox, Allocator, OwnedGlobalPtr},
    marker::{BaseType, Invariant},
    tx::{Result, TxCell, TxHandle, TxObject, TxRef},
};

pub struct ListNode<T: Invariant, A: Allocator> {
    value: T,
    next: Option<InvBox<Self, A>>,
    alloc: A,
}

impl<T: Invariant, A: Allocator> BaseType for ListNode<T, A> {}
unsafe impl<T: Invariant, A: Allocator> Invariant for ListNode<T, A> {}

impl<T: Invariant, A: Allocator + Clone> ListNode<T, A> {
    pub fn new(
        value: T,
        next: Option<OwnedGlobalPtr<Self, A>>,
        alloc: A,
    ) -> crate::tx::Result<OwnedGlobalPtr<Self, A>> {
        todo!()
    }

    pub fn new_inplace<F>(
        next: Option<OwnedGlobalPtr<Self, A>>,
        alloc: A,
        ctor: F,
    ) -> crate::tx::Result<OwnedGlobalPtr<Self, A>>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        todo!()
    }
}

mod tests {
    use super::*;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaObject},
        object::{ObjectBuilder, TypedObject},
    };

    fn simple() {
        let arena = ArenaObject::new();
        let node0 = ListNode::new(3, None, arena.allocator()).unwrap();
        let node1 = ListNode::new(2, Some(node0), arena.allocator()).unwrap();
        let node2 = ListNode::new(1, Some(node1), arena.allocator()).unwrap();

        let rnode2 = node2.resolve();
        let rnode1 = rnode2.next.as_ref().unwrap().resolve();
        let rnode0 = rnode1.next.as_ref().unwrap().resolve();

        assert_eq!(rnode2.value, 1);
        assert_eq!(rnode1.value, 2);
        assert_eq!(rnode0.value, 3);
    }

    fn with_boxes() {
        struct Node {
            data: InvBox<u32, ArenaAllocator>,
        }

        impl Node {
            fn new(value: u32, alloc: ArenaAllocator) -> Self {
                todo!()
            }
        }
        // This would come from derive(Invariant)
        unsafe impl Invariant for Node {}

        let arena = ArenaObject::new();
        let data0 = arena.alloc(3);
        let node0 = ListNode::new_inplace(None, arena.allocator(), |tx| {
            let node = Node {
                data: InvBox::new(tx.tx(), data0),
            };
            tx.write(node)
        })
        .unwrap();
        let node1 = ListNode::new(
            Node::new(2, arena.allocator()),
            Some(node0),
            arena.allocator(),
        )
        .unwrap();
        let node2 = ListNode::new(
            Node::new(1, arena.allocator()),
            Some(node1),
            arena.allocator(),
        )
        .unwrap();

        let rnode2 = node2.resolve();
        let rnode1 = rnode2.next.as_ref().unwrap().resolve();
        let rnode0 = rnode1.next.as_ref().unwrap().resolve();

        assert_eq!(*rnode2.value.data.resolve(), 1);
        assert_eq!(*rnode1.value.data.resolve(), 2);
        assert_eq!(*rnode0.value.data.resolve(), 3);
    }
}
