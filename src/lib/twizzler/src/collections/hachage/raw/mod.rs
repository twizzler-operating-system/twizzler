use std::{alloc::{handle_alloc_error, AllocError, Layout}, collections::TryReserveError, marker::PhantomData, mem::{self, needs_drop, MaybeUninit}, ops::{RangeBounds, RangeInclusive}, ptr::NonNull};

use twizzler_rt_abi::error::{ArgumentError, RawTwzError, TwzError, ResourceError};

use crate::{
    alloc::{Allocator, SingleObjectAllocator}, collections::hachage::table, marker::{BaseType, Invariant, StoreCopy}, ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut}, tx::{Result, TxCell, TxHandle, TxObject, TxRef}
};
use crate::collections::hachage::control::{*};

// Be careful with this library, I stole it. 

#[inline]
#[allow(clippy::cast_possible_truncation)]
fn h1(hash: u64) -> usize {
    // On 32-bit platforms we simply ignore the higher hash bits.
    hash as usize
}

#[derive(Clone)]
struct ProbeSeq {
    pos: usize,
    stride: usize,
}

impl ProbeSeq {
    #[inline]
    fn move_next(&mut self, bucket_mask: usize) {
        debug_assert!(
            self.stride <= bucket_mask,
            "Went past end of probe sequence"
        );

        self.stride += Group::WIDTH;
        self.pos += self.stride;
        self.pos &= bucket_mask;
    }
}

// Stores information about the pointers in the RawTableInner
// Cause no pointer artihmetic and didn't wanna deal with that. 
struct TableLayout {
    size: usize,
    ctrl_align: usize,
    ctrl_offset: usize,
    layout: Option<Layout>
}

impl TableLayout {
    const fn new<T>() -> Self {
        let layout = Layout::new::<T>();

        Self {
            size: layout.size(),
            ctrl_align: if layout.align() > Group::WIDTH {
                layout.align()
            }
            else {
                Group::WIDTH
            },
            ctrl_offset: 0,
            layout: None
        }
    }

    fn new_buckets<T>(buckets: usize) -> Option<Self> {
        let mut table_layout = TableLayout::new::<T>();

        table_layout.update_buckets(buckets)?;

        Some(table_layout)
    }

    fn update_buckets(&mut self, buckets: usize) -> Option<()> {
        let ctrl_offset = self.size.checked_mul(buckets)?
            .checked_add(self.ctrl_align - 1)? & !(self.ctrl_align - 1);

        let len  = ctrl_offset.checked_add(buckets + Group::WIDTH)?;

        if len > isize::MAX as usize - (self.ctrl_align - 1) {
            return None;
        }

        self.ctrl_offset = ctrl_offset;
        self.layout = Some(unsafe { Layout::from_size_align_unchecked(len, self.ctrl_align) });  

        Some(())
    }

    fn ctrl_offset(&self) -> usize {
        self.ctrl_offset
    }

    fn layout(&self) -> Option<Layout> {
        self.layout
    }

    fn layout_ctrl_pair(&self) -> Option<(Layout, usize)> {
        Some(( self.layout()?, self.ctrl_offset()))
    }
}

// Group::WIDTH is added because SIMD stuff. 
fn calculate_layout<T: Invariant>(buckets: usize) -> Option<(Layout, Layout)> {
    Some(unsafe {(
        Layout::array::<T>(buckets).ok()?,
        Layout::array::<Tag>(buckets + Group::WIDTH).ok()?,
    )})
}

// I want to keep the load factor at 90% 
// Though this number doesn't really matter under a page 
fn bucket_mask_to_capacity(bucket_mask: usize) -> usize {
    if bucket_mask < 128 {
        bucket_mask
    } else {
        ((bucket_mask + 1) / 10) * 9
    }
}

fn capacity_to_buckets(proposed_capacity: usize) -> Option<usize> {
    Some(proposed_capacity.next_power_of_two())
}

pub struct RawTableAlloc; 

impl Allocator for RawTableAlloc {
    fn alloc(&self, layout: Layout) -> std::result::Result<GlobalPtr<u8>, AllocError> {
        todo!()
    }

    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: Layout) {
        todo!()
    }
}

pub struct RawTable<T: Invariant, A: Allocator> {
    table: TxCell<RawTableInner>,
    table_layout: TableLayout,
    alloc: A,
    marker: PhantomData<T>
}

impl<T: Invariant, A: Allocator> RawTable<T, A> {
    const TABLE_LAYOUT: TableLayout = TableLayout::new::<T>();

    pub const fn new_in(alloc: A) -> Self {
        Self {
            table: TxCell::new(RawTableInner::new()),
            table_layout: Self::TABLE_LAYOUT,
            alloc: alloc,
            marker: PhantomData
        }
    }

    unsafe fn new_uninitialized(
        alloc: A,
        buckets: usize,
        tx: &TxObject<()>,
    ) -> Result<Self> {
        debug_assert!(buckets.is_power_of_two());
        let table_layout = TableLayout::new_buckets::<T>(buckets).ok_or(TwzError::Resource((ResourceError::OutOfResources)))?;

        Ok(Self {
            table: TxCell::new(RawTableInner::new_uninitialized::<A>(
                &alloc,
                table_layout.layout().unwrap(),
                buckets,
                tx,
            )?),
            table_layout: table_layout,
            alloc: alloc,
            marker: PhantomData,
        })
    }

    pub fn with_capacity_in(alloc: A, capacity: usize, tx: &TxObject<()>) -> Result<Self> {
        if capacity == 0 {return Ok(Self::new_in(alloc))}

        let capacity = capacity_to_buckets(capacity).ok_or(TwzError::Resource(ResourceError::OutOfResources))?;

        let result = unsafe { Self::new_uninitialized(alloc, capacity, tx)? };

        result.empty_ctrl_slice(tx);

        Ok(result)
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        &self.alloc
    }

    pub fn allocation_size(&self) -> usize {
        todo!()
    }

    pub fn find_inner(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<usize> {
        /*let tag_hash = Tag::full(hash);
        let mut probe_seq = self.probe_seq(hash);
        let ctrl_slice = self.table.get_ctrl_slice();


        loop {
            // This assumes control bytes are initialized
            let probed_tag = ctrl_slice.get(probe_seq.pos)?;
            
            // If h2 matches then it checks if the elements match and return it's index
            if (probed_tag.eq(&tag_hash) && eq(self.table.get_data_slice().get(probe_seq.pos)?)) {
                return Some(probe_seq.pos)
            }
            
            // If the probe reaches an empty tag, that means the element doesn't exist. 
            if (probed_tag.special_is_empty()) {
                return None;
            }


            probe_seq.move_next(self.table.bucket_mask);
        }*/
        todo!()
    }

    pub fn get(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<Ref<T>> {
        let index = self.find_inner(hash, eq)?;
        todo!()
        //self.table.get_data_slice().get_ref(index)
    }

    pub fn get_mut(&self, tx: impl AsRef<TxObject>, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Result<Option<RefMut<T>>> {
        /*if let Some(idx) = self.find_inner(hash, eq) {
            self.table.get_data_slice()
                .get_ref(idx)
                .map(|f| f.owned().tx(tx.as_ref()))
                .transpose()
        }
        else {
            Err(twizzler_rt_abi::error::NamingError::NotFound.into())
        }*/
        todo!()
    }

    pub fn capacity(&self) -> usize {
        self.table.items + self.table.growth_left
    }

    pub fn len(&self) -> usize {
        self.table.items
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn buckets(&self) -> usize {
        self.table.bucket_mask + 1
    }

    pub unsafe fn iter(&self) {
        todo!()
    }

    pub fn probe_seq(&self, hash: u64) -> ProbeSeq {
        ProbeSeq {
            pos: h1(hash) & self.table.bucket_mask,
            stride: 0
        }
    }

    /*#[inline]
    pub unsafe fn bucket_index(&self, bucket: &Bucket<T>) -> usize {
        todo!()
    }*/

    // Clear outs the ctrl slices
    fn empty_ctrl_slice(&self, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        todo!()
    }

    fn get_ctrl_offset() -> usize {
        todo!()
    }
    
    fn get_ctrl_slice() {
        todo!()
    }

    fn reserve_rehash(
        &mut self,
        alloc: &A,
        additional: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        drop: Option<unsafe fn(*mut u8)>,
        tx: impl AsRef<TxObject>
    ) -> Result<()> {
        let new_items = self.table.items.checked_add(additional)
            .ok_or(TwzError::Resource(ResourceError::OutOfResources))?;
        let full_capacity = bucket_mask_to_capacity(self.table.bucket_mask);
        if new_items <= full_capacity / 2 {
            // self.rehash_in_place()
        } else {
            // self.resize()
        }

        Ok(())
    }

    fn resize_inner(
        &mut self,
        alloc: &A,
        capacity: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        tx: impl AsRef<TxObject>
    ) -> Result<()> {
        Ok(())
    }

    fn prepare_resize(
        &mut self,
        alloc: &A,
        capacity: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        tx: impl AsRef<TxObject>
    ) -> Result<()> {
        Ok(())
    }


}

/*
 * The seperation between RawTableInner and RawTable is that 
 * RawTableInner performs allocation and access control without any comprehension of the data
 * inside the table. Meaning that it can find, or remove logical data and control elements,
 * but doesn't understand what the data actually means.
 *
 * This means that different implementations of RawTable can operate on the same underlying data structure.
 * 
 * Though the outer data structure has to do a lot more work compared to ordinary hashbrown. 
 * So I don't know if this abstraction is necessary or natural, but I spent a week on this already and want to just
 * code and not think. 
 * 
 */
pub struct RawTableInner {
    data: InvPtr<u8>,
    bucket_mask: usize, 
    growth_left: usize,
    items: usize,
}

impl RawTableInner {
    // Creates a new empty hashtable without allocating memory.
    pub const fn new() -> Self {
        Self {
            // technically there needs to be one bucket so this is incorrect
            // however I didn't want new() to allocate anything yet. Which might be
            // impossible with invariant ptr
            data: InvPtr::null(),
            bucket_mask: 0,
            items: 0,
            growth_left: 0,
        }
    }

    fn do_realloc<A: Allocator>(
        &mut self,
        new_size: usize,
        layout: Layout,
        alloc: &A,
        tx: &TxObject<()>
    ) -> Result<()> {
        let global = alloc.realloc_tx(self.data.global(), layout, new_size, tx)?;

        self.data = InvPtr::new(tx, global.cast())?;

        Ok(())
    }
    
    pub unsafe fn new_uninitialized<A: Allocator>(
        alloc: &A,
        layout: Layout,
        buckets: usize,
        tx: &TxObject<()>,
    ) -> Result<Self>  {
        debug_assert!(buckets.is_power_of_two());
        
        let data_ptr = alloc.alloc_tx(layout, tx)?;

        Ok(Self {
            data: InvPtr::new(tx, data_ptr.cast())?,
            bucket_mask: buckets - 1, 
            items: 0,
            growth_left: bucket_mask_to_capacity(buckets - 1)
        })
    }

    #[inline]
    fn buckets(&self) -> usize {
        self.bucket_mask + 1
    }
}

// Concurrency is gonna suck :p
pub(crate) struct FullBucketsIndices {
    current_group: BitMaskIter,
    group_first_index: usize,
    ctrl: NonNull<u8>, // hmm
    items: usize,
}

/*


    fn with_capacity<A: Allocator>(
        alloc: &A,
        capacity: usize,
        tx: &TxObject<()>,
    ) -> Result<Self> {
        if capacity == 0 {
            return Ok(Self::new());
        }

        let buckets = capacity_to_buckets(capacity).ok_or(TwzError::Resource(ResourceError::OutOfMemory))?;

        let mut result = unsafe { Self::new_uninitialized(alloc, buckets, tx)? };

        Ok(result)
    }
*/