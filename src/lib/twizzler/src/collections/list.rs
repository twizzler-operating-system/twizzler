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
        tx: impl AsRef<TxObject>,
        value: T,
        next: Option<OwnedGlobalPtr<Self, A>>,
        alloc: A,
    ) -> crate::tx::Result<Self> {
        Ok(Self {
            value,
            next: next.map(|n| InvBox::new(tx.as_ref(), n)),
            alloc,
        })
    }
}

mod tests {
    use super::*;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaObject},
        object::{ObjectBuilder, TypedObject},
    };

    #[test]
    fn simple() {
        let arena = ArenaObject::new();
        let alloc = arena.allocator();
        let tx = arena.tx().unwrap();
        let node0 = tx
            .alloc(ListNode::new(&tx, 3, None, alloc).unwrap())
            .unwrap();
        let node1_val = ListNode::new(&tx, 2, Some(node0), alloc).unwrap();
        let node1 = tx.alloc(node1_val).unwrap();
        let node2_val = ListNode::new(&tx, 1, Some(node1), alloc).unwrap();
        let node2 = tx.alloc(node2_val).unwrap();

        let rnode2 = node2.resolve();
        let rnode1 = rnode2.next.as_ref().unwrap().resolve();
        let rnode0 = rnode1.next.as_ref().unwrap().resolve();

        assert_eq!(rnode2.value, 1);
        assert_eq!(rnode1.value, 2);
        assert_eq!(rnode0.value, 3);
    }

    #[test]
    fn with_boxes() {
        struct Node {
            data: InvBox<u32, ArenaAllocator>,
        }

        impl Node {
            fn new(
                tx: impl AsRef<TxObject>,
                value: u32,
                alloc: ArenaAllocator,
            ) -> crate::tx::Result<Self> {
                todo!()
            }
        }
        // This would come from derive(Invariant)
        unsafe impl Invariant for Node {}

        let arena = ArenaObject::new();
        let alloc = arena.allocator();
        let data0 = arena.alloc(3);
        let tx = arena.tx().unwrap();
        let node0 = ListNode::new(&tx, Node::new(&tx, 3, alloc).unwrap(), None, alloc).unwrap();
        let node0 = tx.alloc(node0).unwrap();
        let node1 = tx
            .alloc(
                ListNode::new(&tx, Node::new(&tx, 2, alloc).unwrap(), Some(node0), alloc).unwrap(),
            )
            .unwrap();
        let node2 = tx
            .alloc(
                ListNode::new(&tx, Node::new(&tx, 1, alloc).unwrap(), Some(node1), alloc).unwrap(),
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
