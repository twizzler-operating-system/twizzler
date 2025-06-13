use core::hash;
use std::{alloc::{handle_alloc_error, AllocError, Layout}, collections::TryReserveError, hash::Hash, intrinsics::ptr_offset_from, marker::PhantomData, mem::{self, needs_drop, MaybeUninit}, ops::{Add, RangeBounds, RangeInclusive}, ptr::{copy_nonoverlapping, NonNull}, slice};

use twizzler_rt_abi::error::{ArgumentError, RawTwzError, TwzError, ResourceError};

use crate::{
    alloc::{Allocator, SingleObjectAllocator}, marker::{BaseType, Invariant, StoreCopy}, ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut}, tx::{Result, TxCell, TxHandle, TxObject, TxRef}
};
use crate::collections::hachage::control::{*};
use crate::collections::hachage::scopeguard::{ScopeGuard, guard};
use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};
use equivalent::Equivalent;

use super::DefaultHashBuilder;

// Be careful with this library, I stole it. 

#[derive(Default)]
pub struct HashTableAlloc(pub ObjID);

impl Allocator for HashTableAlloc {
    fn alloc(
        &self,
        layout: Layout,
    ) -> std::result::Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        // 1 for null page, 2 for metadata pages, 1 for base
        if layout.size() > MAX_SIZE - NULLPAGE_SIZE * 4 {
            return Err(std::alloc::AllocError);
        }
        /*let obj = twizzler_rt_abi::object::twz_rt_get_object_handle((self as *const Self).cast())
            .unwrap();*/
        Ok(GlobalPtr::new(self.0, (NULLPAGE_SIZE * 2) as u64))
    }

    unsafe fn dealloc(&self, _ptr: crate::ptr::GlobalPtr<u8>, _layout: Layout) {}
}

impl SingleObjectAllocator for HashTableAlloc {}

#[inline]
#[allow(clippy::cast_possible_truncation)]
fn h1(hash: u64) -> usize {
    // On 32-bit platforms we simply ignore the higher hash bits.
    hash as usize
}

#[derive(Copy, Clone)]
pub struct TableLayout {
    size: usize,
    ctrl_align: usize,
}

impl TableLayout {
    pub const fn new<T>() -> Self {
        let layout = Layout::new::<T>();
        Self {
            size: layout.size(),
            ctrl_align: if layout.align() > Group::WIDTH {
                layout.align()
            } else {
                Group::WIDTH
            },
        }
    }

    fn calculate_layout_for(self, buckets: usize) -> Option<(Layout, usize)> {
        debug_assert!(buckets.is_power_of_two());

        let TableLayout { size, ctrl_align } = self;
        // Manual layout calculation since Layout methods are not yet stable.
        let ctrl_offset =
            size.checked_mul(buckets)?.checked_add(ctrl_align - 1)? & !(ctrl_align - 1);
        let len = ctrl_offset.checked_add(buckets + Group::WIDTH)?;

        // We need an additional check to ensure that the allocation doesn't
        // exceed `isize::MAX` (https://github.com/rust-lang/rust/pull/95295).
        if len > isize::MAX as usize - (ctrl_align - 1) {
            return None;
        }

        Some((
            unsafe { Layout::from_size_align_unchecked(len, ctrl_align) },
            ctrl_offset,
        ))
    }
}

struct ProbeSeq {
    pos: usize,
    stride: usize
}

impl ProbeSeq {
    fn move_next(&mut self, bucket_mask: usize) {
        self.stride += Group::WIDTH;
        self.pos += self.stride;
        self.pos &= bucket_mask;
    }
}

struct LinearProbeSeq {
    pos: usize
}

impl LinearProbeSeq {
    fn move_next(&mut self, bucket_mask: usize) {
        self.pos += 1;
        self.pos &= bucket_mask
    }
}

fn capacity_to_buckets(cap: usize, table_layout: TableLayout) -> Option<usize> {
    debug_assert_ne!(cap, 0);

    // For small tables we require at least 1 empty bucket so that lookups are
    // guaranteed to terminate if an element doesn't exist in the table.
    if cap < 15 {
        // Consider a small TableLayout like { size: 1, ctrl_align: 16 } on a
        // platform with Group::WIDTH of 16 (like x86_64 with SSE2). For small
        // bucket sizes, this ends up wasting quite a few bytes just to pad to
        // the relatively larger ctrl_align:
        //
        // | capacity | buckets | bytes allocated | bytes per item |
        // | -------- | ------- | --------------- | -------------- |
        // |        3 |       4 |              36 | (Yikes!)  12.0 |
        // |        7 |       8 |              40 | (Poor)     5.7 |
        // |       14 |      16 |              48 |            3.4 |
        // |       28 |      32 |              80 |            3.3 |
        //
        // In general, buckets * table_layout.size >= table_layout.ctrl_align
        // must be true to avoid these edges. This is implemented by adjusting
        // the minimum capacity upwards for small items. This code only needs
        // to handle ctrl_align which are less than or equal to Group::WIDTH,
        // because valid layout sizes are always a multiple of the alignment,
        // so anything with alignment over the Group::WIDTH won't hit this edge
        // case.

        // This is brittle, e.g. if we ever add 32 byte groups, it will select
        // 3 regardless of the table_layout.size.
        let min_cap = match (Group::WIDTH, table_layout.size) {
            (16, 0..=1) => 14,
            (16, 2..=3) => 7,
            (8, 0..=1) => 7,
            _ => 3,
        };
        let cap = min_cap.max(cap);
        // We don't bother with a table size of 2 buckets since that can only
        // hold a single element. Instead, we skip directly to a 4 bucket table
        // which can hold 3 elements.
        return Some(if cap < 4 {
            4
        } else if cap < 8 {
            8
        } else {
            16
        });
    }

    // Otherwise require 1/8 buckets to be empty (87.5% load)
    //
    // Be careful when modifying this, calculate_layout relies on the
    // overflow check here.
    let adjusted_cap = cap.checked_mul(8)? / 7;

    // Any overflows will have been caught by the checked_mul. Also, any
    // rounding errors from the division above will be cleaned up by
    // next_power_of_two (which can't overflow because of the previous division).
    Some(adjusted_cap.next_power_of_two())
}

/// Returns the maximum effective capacity for the given bucket mask, taking
/// the maximum load factor into account.
#[inline]
fn bucket_mask_to_capacity(bucket_mask: usize) -> usize {
    if bucket_mask < 8 {
        // For tables with 1/2/4/8 buckets, we always reserve one empty slot.
        // Keep in mind that the bucket mask is one less than the bucket count.
        bucket_mask
    } else {
        // For larger tables we reserve 12.5% of the slots as empty.
        ((bucket_mask + 1) / 8) * 7
    }
}


pub struct RawTable<T: Invariant, S = DefaultHashBuilder, A: Allocator = HashTableAlloc> {
    inner: TxCell<RawTableInner<T>>,
    hasher: S,
    alloc: A,
    _phantom: PhantomData<T>,
}

impl<T: Invariant, S, A: Allocator> BaseType for RawTable<T, S, A> {}

impl<T: Invariant> RawTable<T, DefaultHashBuilder, HashTableAlloc> {
    pub fn new() -> Self {
        Self {
            inner: TxCell::new(RawTableInner::new()),
            hasher: DefaultHashBuilder::default(),
            alloc: HashTableAlloc::default(),
            _phantom: PhantomData
        }
    }
}

impl<T: Invariant, S, A: Allocator> RawTable<T, S, A> {
    const TABLE_LAYOUT: TableLayout = TableLayout::new::<T>();

    pub fn print_slice(&self) {
        println!("{:?}", self.inner.ctrl_slice().as_slice());
    }

    pub const fn hasher(&self) -> &S {
        &self.hasher
    }

    pub const fn allocator(&self) -> &A {
        &self.alloc
    }

    pub const fn with_hasher_in(hasher: S, alloc: A) -> Self {
        Self {
            inner: TxCell::new(RawTableInner::new()),
            hasher: hasher,
            alloc: alloc,
            _phantom: PhantomData
        }
    }

    unsafe fn new_uninitialized(
        hasher: S,
        alloc: A,
        buckets: usize,
        tx: impl AsRef<TxObject>,
    ) -> Result<Self> {
        debug_assert!(buckets.is_power_of_two());

        Ok(Self {
            inner: TxCell::new(RawTableInner::new_uninitialized(
                &alloc, 
                buckets, 
                tx.as_ref()
            )?),
            hasher: hasher,
            alloc,
            _phantom: PhantomData,
        })
    }

    pub fn with_capacity_in(
        hasher: S,
        alloc: A,
        capacity: usize, 
        tx: impl AsRef<TxObject>,
    ) -> Self {
        let foo = Self {
            inner: TxCell::new(RawTableInner::with_capacity(
                &alloc, 
                capacity,
                tx.as_ref()
            ).unwrap()),
            hasher: hasher,
            alloc,
            _phantom: PhantomData,
        };

        foo
    }

    pub unsafe fn insert_in_slot(&self, hash: u64, slot: usize, value: T, tx: impl AsRef<TxObject>) -> Ref<T> {
        let inner = self.inner.get_mut(tx.as_ref()).unwrap();
        inner.with_mut_slice(tx.as_ref(), |c, s| {
            c[slot] = Tag::full(hash);
            s[slot] = value;
            Ok(self.bucket(slot))
        }).unwrap()
    }

    pub fn find(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<Ref<T>> {
        unsafe { 
            let result = self
                .inner
                .find_inner(hash, &mut |index| eq(&self.inner.bucket(index)));

            match result {
                Some(index) => Some(self.inner.bucket(index)),
                None => None,
            }
        }
    }

    pub fn get(&self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<&T> {
        match self.find(hash, eq) {
            Some(r) => unsafe { r.raw().as_ref() },
            None => None,
        }
    }

    pub fn insert(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64, tx: impl AsRef<TxObject>) -> Ref<T> {
        let handle = self.inner.get_mut(tx.as_ref()).unwrap();
        handle.insert(hash, value, hasher, tx)
    }

    pub fn find_or_find_insert_slot(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        hasher: impl Fn(&T) -> u64,
        tx: impl AsRef<TxObject>
    ) -> std::result::Result<Ref<T>, usize> {
        self.reserve(1, hasher, tx);

        match unsafe { self.inner.find_or_find_insert_slot_inner(hash, &mut |index| eq(&self.bucket(index))) } {
            Ok(index) => { Ok(self.bucket(index))},
            Err(slot) => Err(slot),
        }
    }

    pub fn reserve(&mut self, additional: usize, hasher: impl Fn(&T) -> u64, tx: impl AsRef<TxObject>) -> Result<()> {
        if additional > self.inner.growth_left {
            unsafe {
                self.reserve_rehash(additional, hasher, tx)
            }
        }
        else {
            Ok(())
        }
    }

    pub fn reserve_rehash(
        &mut self,
        additional: usize, 
        hasher: impl Fn(&T) -> u64,
        tx: impl AsRef<TxObject>,
    ) -> Result<()> {
        let foo = self.inner.get_mut(tx.as_ref())?; 
        unsafe { foo.reserve_rehash_inner(
            &self.alloc,
            additional, 
            &|table, index| hasher(&*table.bucket(index)), 
            tx
        ) }
    }

    pub fn bucket(&self, index: usize) -> Ref<T> {
        unsafe { self.inner.bucket(index) }
    }
}

pub struct RawTableInner<T: Invariant> {
    ctrl: InvPtr<u8>,
    data: InvPtr<u8>,
    bucket_mask: usize,
    growth_left: usize,
    items: usize,
    _pde: PhantomData<T>
}

impl<T: Invariant> RawTableInner<T> {
    pub const fn new() -> Self {
        Self {
            ctrl: InvPtr::null(),
            data: InvPtr::null(),
            bucket_mask: 0,
            growth_left: 0,
            items: 0,
            _pde: PhantomData
        }
    }

    pub const fn len(&self) -> usize {
        self.items
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Invariant> RawTableInner<T> {
    pub unsafe fn new_uninitialized<A: Allocator>(
        alloc: &A,
        buckets: usize,
        tx: &TxObject<()>,
    ) -> Result<Self>
    where
        A: Allocator,
    {
        debug_assert!(buckets.is_power_of_two());
        
        // Avoid `Option::ok_or_else` because it bloats LLVM IR.
        let (layout, ctrl_offset) = TableLayout::new::<T>().calculate_layout_for(buckets)
            .ok_or(TwzError::Uncategorized(0))?;

        let ptr = alloc.alloc(layout)?;
        let r = InvPtr::new(tx, ptr).map_err(|x| {
            // on the off chance that the FOT runs out, but you can still allocate bytes
            alloc.dealloc(ptr, layout);
            x
        })?;

        println!("created inv ptr: {:?}", r);
        
        // SAFETY: null pointer will be caught in above check
        Ok(Self {
            ctrl: r,
            data: InvPtr::null(),
            bucket_mask: buckets - 1,
            items: 0,
            growth_left: bucket_mask_to_capacity(buckets - 1),
            _pde: PhantomData
        })
    }

    fn with_capacity<A: Allocator>(
        alloc: &A,
        capacity: usize,
        tx: &TxObject<()>,
    ) -> Result<Self> {
        if capacity == 0 {
            return Ok(Self::new())
        }
        
        unsafe {
            let buckets = capacity.next_power_of_two();
            /*capacity_to_buckets(capacity, table_layout)
                .ok_or(TwzError::Uncategorized(0))?;*/

            let mut result = Self::new_uninitialized(alloc, buckets, tx)?;

            // Data races can't happen yet since the table isn't visible anywhere yet 
            let slice = result.ctrl_slice_raw();
            slice.fill_empty();
            println!("{:?}", slice);
            Ok(result)
        }
    }

    fn probe_seq(&self, hash: u64) -> LinearProbeSeq {
        LinearProbeSeq {
            pos: h1(hash) & self.bucket_mask,
        }
    }

    pub fn find(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<Ref<T>> {
        unsafe { 
            let result = self
                .find_inner(hash, &mut |index| eq(&self.bucket(index)));

            match result {
                Some(index) => Some(self.bucket(index)),
                None => None,
            }
        }
    }

    unsafe fn find_inner(&self, hash: u64, eq: &mut dyn FnMut(usize) -> bool) -> Option<usize> { 
        debug_assert!(!self.is_empty());
        let tag_hash = Tag::full(hash);
        let mut probe_seq = self.probe_seq(hash);
        let ctrl_slice = self.ctrl_slice();
        for i in ctrl_slice.as_slice() {
            print!("{:?} ", i)
        }
        println!("");
        loop {
            let tag = ctrl_slice.get(probe_seq.pos)?;
            
            if tag.eq(&tag_hash) && eq(probe_seq.pos) {
                return Some(probe_seq.pos);
            }

            println!("{:?}", tag);
            if tag.special_is_empty() {
                return None;
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    unsafe fn find_insert_slot(&self, hash: u64) -> usize {
        let mut probe_seq = self.probe_seq(hash);
        let ctrl_slice = self.ctrl_slice();

        loop {
            let tag = ctrl_slice.get(probe_seq.pos).unwrap();
            if tag.is_special() {
                return probe_seq.pos;
            }
            
            probe_seq.move_next(self.bucket_mask);
        }
    }

    pub fn insert(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64, tx: impl AsRef<TxObject>) -> Ref<T> {
        let ctrl = self.ctrl_slice();
        unsafe {
            let mut slot = self.find_insert_slot(hash);

            let old_tag = ctrl.get(slot).unwrap();
            if (self.growth_left == 0 && old_tag.special_is_empty()) {
                todo!()
            }

            let old_ctrl = ctrl.get(slot).unwrap();
            self.growth_left -= usize::from(old_ctrl.special_is_empty());
            self.items += 1;
            self.with_mut_slice(tx.as_ref(), |c, s| {
                c[slot] = Tag::full(hash);
                s[slot] = value;
                Ok(())
            }).unwrap();
            self.data_slice().get_ref(slot).unwrap()
        }
    }

    unsafe fn find_or_find_insert_slot_inner(
        &self,
        hash: u64,
        eq: &mut dyn FnMut(usize) -> bool,
    ) -> std::result::Result<usize, usize> {
        let tag_hash = Tag::full(hash);
        let mut probe_seq = self.probe_seq(hash);

        let ctrl = self.ctrl_slice();

        loop {
            let tag = ctrl.get(probe_seq.pos).unwrap();

            if tag.eq(&tag_hash) && eq(probe_seq.pos) {
                return Ok(probe_seq.pos)
            }

            // If the tag is empty that means that the index couldn't be in subsequent elements
            // since if the tag was deleted there's a still a change the element could be 
            // further down the line
            println!("{:?}", tag);
            if tag.special_is_empty() {
                return Err(probe_seq.pos)
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    unsafe fn reserve_rehash_inner<A: Allocator>(
        &mut self, 
        alloc: &A,
        additional: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        tx: impl AsRef<TxObject>
    ) -> Result<()> {
        let new_items = self.items.checked_add(additional).ok_or(TwzError::Uncategorized(0))?;
        let full_capacity = bucket_mask_to_capacity(self.bucket_mask);

        self.resize_inner(
            alloc, 
            usize::max(new_items, full_capacity + 1), 
            hasher, 
            tx
        )
    }


    fn prepare_resize<'a, A: Allocator>(
        &self, 
        alloc: &'a A, 
        capacity: usize,
        tx: impl AsRef<TxObject>
    ) -> Result<ScopeGuard<Self, impl FnMut(&mut Self) + 'a>> {
                debug_assert!(self.items <= capacity);

        let new_table = RawTableInner::with_capacity(alloc, capacity, tx.as_ref())?;

        // I still don't know what dropping really means in a persistent enviornment,
        // like it would be hell to manage right? 
        Ok(guard(new_table, move |self_| {
            todo!()
        }))
    }

    // I'm using alloc instead of realloc here, making an assumption about how the allocator works
    // but this gets me half the potential capacity of an object I need to do a rehash in place
    // instead but with two different sizes on the same array? 
    // But once I create a sharding wrapper the amount a single object holds matters less. 
    unsafe fn resize_inner<A: Allocator>(
        &mut self, 
        alloc: &A,
        capacity: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        tx: impl AsRef<TxObject>
    ) -> Result<()> {
        let mut new_table = self.prepare_resize(alloc, capacity, tx)?;
        // we don't have to worry about concurrency here since only this function
        // has access 
        for i in 0..self.buckets() {
            let hash = hasher(self, i);

            let slot = new_table.find_insert_slot(hash);
            *new_table.ctrl(slot) = Tag::full(hash);
            
            std::ptr::copy_nonoverlapping::<T>(
                self.bucket_ptr(i),
                new_table.bucket_ptr(slot),
                1
            );
        }

        new_table.growth_left -= self.items;
        new_table.items = self.items;

        mem::swap(self, &mut new_table);

        Ok(())
    }

    fn get_data(&mut self, _tx: &TxObject<()>) -> RefMut<u8> {
        todo!()
    }

    unsafe fn ctrl_slice_raw(&mut self) -> &mut [Tag] {
        std::slice::from_raw_parts_mut(
            self.ctrl.resolve()
                .cast::<Tag>()
                .mutable()
                .raw(), 
            self.buckets()
        )
    }

    fn ctrl_slice(&self) -> RefSlice<Tag> {
        let r: Ref<'_, Tag> = unsafe { self.ctrl.resolve().cast() };
        let slice = unsafe { RefSlice::from_ref(r, self.buckets()) };
        slice 
    }

    fn mut_ctrl_slice<R>(
        &mut self, 
        tx: &TxObject<()>, 
        f: impl FnOnce(&mut [Tag]) -> crate::tx::Result<R>
    ) -> crate::tx::Result<R> {
        let r: Ref<'_, Tag> = unsafe { self.ctrl.resolve().cast() };
        let mut slice = unsafe { RefSlice::from_ref(r, self.buckets()).tx(0..self.buckets(), tx)? };
        f(slice.as_slice_mut())
    }

    unsafe fn data_slice_raw(&mut self) -> &mut [T] {
        let r = self.data();

        std::slice::from_raw_parts_mut(r, self.buckets())
    }

    #[inline]
    fn data_slice(&self) -> RefSlice<'_, T> {
        let r: Ref<'_, T> = unsafe { self.ctrl.add(self.buckets()).resolve().owned().cast()};
        let slice = unsafe { RefSlice::from_ref(r, self.buckets())};
        slice
    }

    pub unsafe fn bucket(&self, index: usize) -> Ref<T> {
        self.data_slice().get_ref(index).unwrap()
    }

    fn buckets(&self) -> usize {
        self.bucket_mask + 1
    }

    fn num_ctrl_bytes(&self) -> usize {
        self.buckets()
    }

    unsafe fn with_mut_slice<R>(
        &mut self,
        tx: &TxObject<()>, 
        f: impl FnOnce(&mut [Tag], &mut [T]) -> crate::tx::Result<R>
    ) -> crate::tx::Result<R> {
        let r = unsafe { self.ctrl.resolve() };
        let byte_length = self.buckets() * (size_of::<T>() + size_of::<Tag>());
        let mut slice = unsafe {
            RefSlice::from_ref(r, byte_length).tx(
                0..byte_length,
                tx
            )?
        };
        let ctrl_ptr = slice.as_slice_mut().as_mut_ptr();
        let ctrl_slice = slice::from_raw_parts_mut(ctrl_ptr.cast(), self.buckets());
        let data_ptr: *mut T = ctrl_ptr.add(self.buckets()).cast();
        let data_slice = unsafe { slice::from_raw_parts_mut(data_ptr, self.buckets())};
        f(ctrl_slice, data_slice)
    }

    // Obtains the data pointer without transaction safety beware!
    unsafe fn bucket_ptr(&self, index: usize) -> *mut T {
        debug_assert!(index < self.buckets());

        self.data().add(index)
    }

    // Obtains the data pointer without transaction safety beware!
    unsafe fn data(&self) -> *mut T {
        self.ctrl.byte_add(self.buckets())
            .cast::<T>()
            .resolve()
            .owned()
            .mutable()
            .raw()
    }

    unsafe fn ctrl(&self, index: usize) -> *mut Tag {
        debug_assert!(index < self.num_ctrl_bytes());

        self.ctrl.add(index)
            .resolve()
            .owned()
            .mutable()
            .raw()
            .cast()
    }
}

/*impl<T: Invariant> std::fmt::Debug for RawTableInner<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for i in self.ctrl_slice().as_slice() {
            f.write_fmt("{:?}", i)
        }

        Ok(())
    }
}*/