use std::{mem::MaybeUninit, ops::RangeBounds};

use twizzler_rt_abi::error::ArgumentError;

use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{Invariant, StoreCopy},
    object::{Object, ObjectBuilder, TypedObject},
    ptr::{Ref, RefSlice},
    tx::TxRef,
};

pub struct TableObject<T: Invariant, A: Allocator> {
	obj: Object<RawTable<T, A>>,
}

pub struct RawTable<T, A: Allocator = Global> {
    table: RawTableInner,
    alloc: A,
    marker: PhantomData<T>,
}

/* 
 * There's a lot of moving parts here. And I stole the vast majority of this code from hashbrown
 * 
 * But a quick rundown on how hashbrown and by extension Google's Swisstable works
 * 
 * https://faultlore.com/blah/hashbrown-tldr/
 * 
 * Some considerations for safety 
 * Panics can often involve data loss when resizing the hashmap.
 *  I assume some transaction mechanism 
*/
struct RawTableInner {
    bucket_mask: usize,

    // [Padding], T_n, ..., T1, T0, C0, C1, ...
    //                              ^ points here
    ctrl: InvPtr<u8>,

    growth_left: usize, 

    items: usize
}