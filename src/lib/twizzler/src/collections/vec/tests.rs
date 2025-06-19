#![allow(dead_code)]
use super::*;
use crate::{
    marker::{BaseType, Invariant},
    object::{ObjectBuilder, TypedObject},
    ptr::{GlobalPtr, InvPtr},
};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Simple {
    x: u32,
}
unsafe impl Invariant for Simple {}

impl BaseType for Simple {}

struct Node {
    pub ptr: InvPtr<Simple>,
}

impl Node {
    pub fn new_inplace(
        place: RefMut<MaybeUninit<Self>>,
        ptr: impl Into<GlobalPtr<Simple>>,
    ) -> Result<RefMut<Self>> {
        let ptr = InvPtr::new(&place, ptr)?;
        Ok(place.write(Self { ptr }))
    }
}

impl BaseType for Node {}
unsafe impl Invariant for Node {}

#[test]
fn simple_push() {
    let vobj = ObjectBuilder::default()
        .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
        .unwrap();

    let mut tx = vobj.into_tx().unwrap();
    tx.base_mut().push(Simple { x: 42 }).unwrap();
    tx.base_mut().push(Simple { x: 43 }).unwrap();
    tx.commit().unwrap();

    let vobj = tx.into_object().unwrap();
    let base = vobj.base();
    assert_eq!(base.len(), 2);
    let item = base.get_ref(0).unwrap();
    assert_eq!(item.x, 42);
    let item2 = base.get_ref(1).unwrap();
    assert_eq!(item2.x, 43);
}

#[test]
fn simple_push_vo() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 42 }).unwrap();

    let item = vec_obj.get_ref(0).unwrap();
    assert_eq!(item.x, 42);
}

#[test]
fn simple_remove_vo() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 42 }).unwrap();

    let item = vec_obj.get_ref(0).unwrap();
    assert_eq!(item.x, 42);
    let ritem = vec_obj.remove(0).unwrap();

    assert_eq!(ritem.x, 42);
}

#[test]
fn multi_remove_vo() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 42 }).unwrap();
    vec_obj.push(Simple { x: 43 }).unwrap();
    vec_obj.push(Simple { x: 44 }).unwrap();

    let item = vec_obj.get_ref(0).unwrap();
    assert_eq!(item.x, 42);
    let item = vec_obj.get_ref(1).unwrap();
    assert_eq!(item.x, 43);
    let item = vec_obj.get_ref(2).unwrap();
    assert_eq!(item.x, 44);
    let item = vec_obj.get_ref(3);
    assert!(item.is_none());

    let ritem = vec_obj.remove(1).unwrap();
    assert_eq!(ritem.x, 43);

    let item = vec_obj.get_ref(0).unwrap();
    assert_eq!(item.x, 42);
    let item = vec_obj.get_ref(1).unwrap();
    assert_eq!(item.x, 44);
    let item = vec_obj.get_ref(2);
    assert!(item.is_none());
}

#[test]
fn many_push_vo() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    for i in 0..100 {
        vec_obj.push(Simple { x: i * i }).unwrap();
    }

    for i in 0..100 {
        let item = vec_obj.get_ref(i as usize).unwrap();
        assert_eq!(item.x, i * i);
    }
}

#[test]
fn node_push() {
    let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
    let vobj = ObjectBuilder::<Vec<Node, VecObjectAlloc>>::default()
        .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
        .unwrap();

    let mut tx = vobj.into_tx().unwrap();
    let mut base = tx.base_mut().owned();
    base.push_inplace(Node {
        ptr: InvPtr::new(&tx, simple_obj.base_ref()).unwrap(),
    })
    .unwrap();
    tx.commit().unwrap();

    let vobj = tx.into_object().unwrap();
    let rbase = vobj.base();
    let item = rbase.get_ref(0).unwrap();
    assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
}

#[test]
fn vec_object() {
    let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
    let mut vo = VecObject::new(ObjectBuilder::default()).unwrap();
    vo.push_ctor(|place| {
        let node = Node {
            ptr: InvPtr::new(&place, simple_obj.base_ref())?,
        };
        Ok(place.write(node))
    })
    .unwrap();

    vo.push_ctor(|place| Node::new_inplace(place, simple_obj.base_ref()))
        .unwrap();

    let base = vo.object().base();
    let item = base.get_ref(0).unwrap();
    assert_eq!(unsafe { item.ptr.resolve().x }, 3);
}

#[test]
fn test_new_empty() {
    let vec_obj = VecObject::<u32, VecObjectAlloc>::new(ObjectBuilder::default()).unwrap();
    assert_eq!(vec_obj.len(), 0);
    assert!(vec_obj.is_empty());
}

#[test]
fn test_capacity_and_reserve() {
    let mut vec_obj = VecObject::<u32, VecObjectAlloc>::new(ObjectBuilder::default()).unwrap();
    let initial_cap = vec_obj.capacity();

    vec_obj.reserve(10).unwrap();
    assert!(vec_obj.capacity() >= initial_cap + 10);
}

#[test]
fn test_clear() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();

    assert_eq!(vec_obj.len(), 2);
    vec_obj.clear().unwrap();
    assert_eq!(vec_obj.len(), 0);
    assert!(vec_obj.is_empty());
}

#[test]
fn test_pop() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 42 }).unwrap();
    vec_obj.push(Simple { x: 43 }).unwrap();

    let popped = vec_obj.pop().unwrap().unwrap();
    assert_eq!(popped.x, 43);
    assert_eq!(vec_obj.len(), 1);

    let popped = vec_obj.pop().unwrap().unwrap();
    assert_eq!(popped.x, 42);
    assert_eq!(vec_obj.len(), 0);

    let empty_pop = vec_obj.pop().unwrap();
    assert!(empty_pop.is_none());
}

#[test]
fn test_truncate() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    for i in 0..10 {
        vec_obj.push(Simple { x: i }).unwrap();
    }

    vec_obj.truncate(5).unwrap();
    assert_eq!(vec_obj.len(), 5);

    for i in 0..5 {
        let item = vec_obj.get_ref(i).unwrap();
        assert_eq!(item.x, i as u32);
    }

    // Truncating to larger size should be no-op
    vec_obj.truncate(10).unwrap();
    assert_eq!(vec_obj.len(), 5);
}

#[test]
fn test_swap() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();

    vec_obj.swap(0, 2).unwrap();

    assert_eq!(vec_obj.get_ref(0).unwrap().x, 3);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 2);
    assert_eq!(vec_obj.get_ref(2).unwrap().x, 1);

    // Swapping same index should be no-op
    vec_obj.swap(1, 1).unwrap();
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 2);
}

#[test]
fn test_first_and_last() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();

    assert!(vec_obj.first_ref().is_none());
    assert!(vec_obj.last_ref().is_none());

    vec_obj.push(Simple { x: 10 }).unwrap();
    assert_eq!(vec_obj.first_ref().unwrap().x, 10);
    assert_eq!(vec_obj.last_ref().unwrap().x, 10);

    vec_obj.push(Simple { x: 20 }).unwrap();
    vec_obj.push(Simple { x: 30 }).unwrap();

    assert_eq!(vec_obj.first_ref().unwrap().x, 10);
    assert_eq!(vec_obj.last_ref().unwrap().x, 30);
}

#[test]
fn test_contains() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();

    let target1 = Simple { x: 2 };
    let target2 = Simple { x: 5 };

    assert!(vec_obj.contains(&target1));
    assert!(!vec_obj.contains(&target2));
}

#[test]
fn test_starts_with_ends_with() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();
    vec_obj.push(Simple { x: 4 }).unwrap();

    let start_needle = [Simple { x: 1 }, Simple { x: 2 }];
    let end_needle = [Simple { x: 3 }, Simple { x: 4 }];
    let wrong_needle = [Simple { x: 5 }, Simple { x: 6 }];

    assert!(vec_obj.starts_with(&start_needle));
    assert!(vec_obj.ends_with(&end_needle));
    assert!(!vec_obj.starts_with(&wrong_needle));
    assert!(!vec_obj.ends_with(&wrong_needle));
}

#[test]
fn test_binary_search() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();
    vec_obj.push(Simple { x: 5 }).unwrap();
    vec_obj.push(Simple { x: 7 }).unwrap();

    let target = Simple { x: 5 };
    let missing = Simple { x: 6 };

    assert_eq!(vec_obj.binary_search(&target), Ok(2));
    assert_eq!(vec_obj.binary_search(&missing), Err(3));
}

#[test]
fn test_reverse() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();

    vec_obj.reverse().unwrap();

    assert_eq!(vec_obj.get_ref(0).unwrap().x, 3);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 2);
    assert_eq!(vec_obj.get_ref(2).unwrap().x, 1);
}

#[test]
fn test_sort() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 4 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();

    vec_obj.sort().unwrap();

    assert_eq!(vec_obj.get_ref(0).unwrap().x, 1);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 2);
    assert_eq!(vec_obj.get_ref(2).unwrap().x, 3);
    assert_eq!(vec_obj.get_ref(3).unwrap().x, 4);
}

#[test]
fn test_sort_unstable() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 5 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 8 }).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();

    vec_obj.sort_unstable().unwrap();

    assert_eq!(vec_obj.get_ref(0).unwrap().x, 1);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 2);
    assert_eq!(vec_obj.get_ref(2).unwrap().x, 5);
    assert_eq!(vec_obj.get_ref(3).unwrap().x, 8);
}

#[test]
fn test_retain() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    for i in 1..=10 {
        vec_obj.push(Simple { x: i }).unwrap();
    }

    // Keep only even numbers
    vec_obj.retain(|item| item.x % 2 == 0).unwrap();

    assert_eq!(vec_obj.len(), 5);
    assert_eq!(vec_obj.get_ref(0).unwrap().x, 2);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 4);
    assert_eq!(vec_obj.get_ref(2).unwrap().x, 6);
    assert_eq!(vec_obj.get_ref(3).unwrap().x, 8);
    assert_eq!(vec_obj.get_ref(4).unwrap().x, 10);
}

#[test]
fn test_with_slice() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();

    let sum = vec_obj.with_slice(|slice| slice.iter().map(|s| s.x).sum::<u32>());

    assert_eq!(sum, 6);
}

#[test]
fn test_with_mut_slice() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();

    vec_obj
        .with_mut_slice(1..3, |slice| {
            for item in slice {
                item.x *= 2;
            }
            Ok(())
        })
        .unwrap();

    assert_eq!(vec_obj.get_ref(0).unwrap().x, 1);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 4);
    assert_eq!(vec_obj.get_ref(2).unwrap().x, 6);
}

#[test]
fn test_shrink_to_fit() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.reserve(100).unwrap();

    for i in 0..10 {
        vec_obj.push(Simple { x: i }).unwrap();
    }

    let old_capacity = vec_obj.capacity();
    vec_obj.shrink_to_fit().unwrap();
    let new_capacity = vec_obj.capacity();

    assert_eq!(new_capacity, vec_obj.len());
    assert!(new_capacity <= old_capacity);
}

#[test]
fn test_remove_inplace() {
    let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
    vec_obj.push(Simple { x: 1 }).unwrap();
    vec_obj.push(Simple { x: 2 }).unwrap();
    vec_obj.push(Simple { x: 3 }).unwrap();

    vec_obj.remove_inplace(1).unwrap();

    assert_eq!(vec_obj.len(), 2);
    assert_eq!(vec_obj.get_ref(0).unwrap().x, 1);
    assert_eq!(vec_obj.get_ref(1).unwrap().x, 3);

    // Test removing from invalid index
    assert!(vec_obj.remove_inplace(10).is_err());
}
