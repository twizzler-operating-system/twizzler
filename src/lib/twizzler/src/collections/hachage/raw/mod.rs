use core::hash;
use std::{
    alloc::{handle_alloc_error, AllocError, Layout},
    collections::TryReserveError,
    hash::Hash,
    intrinsics::ptr_offset_from,
    marker::PhantomData,
    mem::{self, needs_drop, MaybeUninit},
    ops::{Add, Index, IndexMut, RangeBounds, RangeInclusive},
    ptr::{copy_nonoverlapping, NonNull},
    slice,
};

use equivalent::Equivalent;
use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    error::{ArgumentError, RawTwzError, ResourceError, TwzError},
    object::ObjectHandle,
};

use super::DefaultHashBuilder;
use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    collections::hachage::{
        control::*,
        scopeguard::{guard, ScopeGuard},
    },
    marker::{BaseType, Invariant, StoreCopy},
    ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut},
    Result,
};

#[derive(Default, Clone, Copy)]
pub struct HashTableAlloc;

impl Allocator for HashTableAlloc {
    fn alloc(
        &self,
        layout: Layout,
    ) -> std::result::Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        // 1 for null page, 2 for metadata pages, 1 for base
        if layout.size() > MAX_SIZE - NULLPAGE_SIZE * 4 {
            return Err(std::alloc::AllocError);
        }
        let obj = twizzler_rt_abi::object::twz_rt_get_object_handle((self as *const Self).cast())
        .unwrap();

        Ok(GlobalPtr::new(obj.id(), (NULLPAGE_SIZE * 2) as u64))
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
struct TableLayout {
    size: usize,
    ctrl_align: usize,
}

impl TableLayout {
    #[inline]
    const fn new<T: Invariant>() -> Self {
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

    fn calculate_data_offset(&self, buckets: usize) -> usize {
        match self.ctrl_align > buckets {
            true => self.ctrl_align,
            false => buckets,
        }
    }

    #[inline]
    fn calculate_layout_for(&self, buckets: usize) -> Option<(Layout, usize)> {
        debug_assert!(buckets.is_power_of_two());

        // The layout is CTRL DATA
        // So we want data_offset to be aligned with the data
        let data_offset = self.calculate_data_offset(buckets);
        let len = data_offset + self.size * buckets;
        // We need an additional check to ensure that the allocation doesn't
        // exceed `isize::MAX` (https://github.com/rust-lang/rust/pull/95295).
        if len > isize::MAX as usize - (self.ctrl_align - 1) {
            return None;
        }

        Some((
            unsafe { Layout::from_size_align_unchecked(len, self.ctrl_align) },
            data_offset,
        ))
    }
}

struct ProbeSeq {
    pos: usize,
    stride: usize,
}

impl ProbeSeq {
    fn move_next(&mut self, bucket_mask: usize) {
        self.stride += Group::WIDTH;
        self.pos += self.stride;
        self.pos &= bucket_mask;
    }
}

struct LinearProbeSeq {
    pos: usize,
}

impl LinearProbeSeq {
    fn new(hash: u64, mask: usize) -> LinearProbeSeq {
        LinearProbeSeq {
            pos: h1(hash) & mask,
        }
    }

    fn move_next(&mut self, bucket_mask: usize) {
        self.pos += 1;
        self.pos &= bucket_mask
    }
}

fn capacity_to_buckets(cap: usize, table_layout: TableLayout) -> Option<usize> {
    debug_assert_ne!(cap, 0);

    Some(cap.next_power_of_two())
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
    table: RawTableInner,
    // I have to keep the hasher state otherwise the hashes won't be the same upon reload
    hasher: S,
    alloc: A,
    _phantom: PhantomData<T>,
}

impl<T: Invariant, S, A: Allocator> BaseType for RawTable<T, S, A> {}

impl<T: Invariant> RawTable<T, DefaultHashBuilder, HashTableAlloc> {
    pub fn new() -> Self {
        Self {
            table: RawTableInner::new(),
            hasher: DefaultHashBuilder::default(),
            alloc: HashTableAlloc::default(),
            _phantom: PhantomData,
        }
    }
}

impl<T: Invariant, S, A: Allocator> RawTable<T, S, A> {
    const TABLE_LAYOUT: TableLayout = TableLayout::new::<T>();

    pub fn print_slice(&self) {
        println!("{:?}", unsafe { self.table.ctrl_slice().as_slice() });
    }

    pub fn hasher(&self) -> &S {
        &self.hasher
    }

    pub fn allocator(&self) -> &A {
        &self.alloc
    }

    pub fn with_hasher_in(hasher: S, alloc: A) -> Self {
        Self {
            table: RawTableInner::new(),
            hasher,
            alloc,
            _phantom: PhantomData,
        }
    }

    pub fn find(&self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<Ref<T>> {
        unsafe {
            let result = self
                .table
                .find_inner(hash, &mut |index| eq(&self.bucket(index)));

            match result {
                Some(index) => {
                    Some(self.bucket(index))
                },
                None => None,
            }
        }
    }

    pub fn get(&self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<&T> {
        //unsafe { println!("{:?}", RefSlice::from_ref(self.table.ctrl.resolve(), self.table.buckets() * (1 + size_of::<T>())).as_slice()); }
        match self.find(hash, eq) {
            Some(r) => unsafe { r.raw().as_ref() },
            None => None,
        }
    }

    pub fn reserve(&mut self, additional: usize, hasher: impl Fn(&T) -> u64) {
        if additional > self.table.growth_left {
            unsafe { self.reserve_rehash(additional, hasher).unwrap() }
        }
    }

    pub fn reserve_rehash(&mut self, additional: usize, hasher: impl Fn(&T) -> u64) -> Result<()> {
        unsafe {
            self.table.reserve_rehash_inner(
                &self.alloc,
                additional,
                &|table, index| hasher(&table.bucket(index)),
                &Self::TABLE_LAYOUT,
                None,
            )
        }
    }

    pub unsafe fn resize(&mut self, capacity: usize, hasher: impl Fn(&T) -> u64) -> Result<()> {        
        self.table.resize_inner(
            &self.alloc,
            capacity,
            &|table, index| hasher(&table.bucket(index)),
            &Self::TABLE_LAYOUT,
        )
    }

    pub fn insert(&mut self, hash: u64, value: T, hasher: impl Fn(&T) -> u64) {
        unsafe {
            let mut index = self.table.find_insert_slot(hash);

            let old_ctrl = self.table.ctrl_slice()[index];
            if self.table.growth_left == 0 && old_ctrl.special_is_empty() {
                self.reserve(1, hasher);
                index = self.table.find_insert_slot(hash);
            }
            self.insert_in_slot(hash, index, value)
        }
    }

    // Returns a reference to a slot or a candidate to insert
    pub fn find_or_find_insert_slot(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        hasher: impl Fn(&T) -> u64,
    ) -> std::result::Result<Ref<T>, usize> {
        self.reserve(1, hasher);
        let foo = unsafe { RefSlice::from_ref(self.table.ctrl.resolve(), size_of::<T>() * (self.table.buckets() + 1))};

        unsafe {
            match self
                .table
                .find_or_find_insert_slot_inner(hash, &mut |index| eq(&self.bucket(index)))
            {
                Ok(index) => Ok(self.bucket(index)),
                Err(slot) => Err(slot),
            }
        }
    }

    pub unsafe fn insert_in_slot(&mut self, hash: u64, slot: usize, value: T) {
        let old_ctrl = self.table.ctrl_slice()[slot];
        self.table.record_item_insert_at(slot, old_ctrl, hash);

        let mut bucket = self.table.bucket::<T>(slot).as_mut();
        *bucket = value;
        //let mut bucket_mut = bucket.as_mut();
    }

    fn bucket(&self, index: usize) -> Ref<T> {
        unsafe {
            self.table
                .data_ref(&Self::TABLE_LAYOUT)
                .cast::<T>()
                .add(index)
        }
    }
}

#[derive(Debug)]
pub struct RawTableInner {
    ctrl: InvPtr<u8>,
    bucket_mask: usize,
    growth_left: usize,
    items: usize,
}

impl RawTableInner {
    pub fn new() -> Self {
        Self {
            ctrl: InvPtr::null(),
            bucket_mask: 0,
            growth_left: 0,
            items: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items
    }

    pub fn buckets(&self) -> usize {
        self.bucket_mask + 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_empty_singleton(&self) -> bool {
        self.bucket_mask == 0
    }
}

impl RawTableInner {
    fn probe_seq(&self, hash: u64) -> LinearProbeSeq {
        LinearProbeSeq {
            pos: h1(hash) & self.bucket_mask,
        }
    }

    unsafe fn record_item_insert_at(&mut self, index: usize, old_ctrl: Tag, hash: u64) {
        self.growth_left -= usize::from(old_ctrl.special_is_empty());
        self.set_ctrl_hash(index, hash);
        self.items += 1;
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
            let tag = ctrl[probe_seq.pos];

            if tag.eq(&tag_hash) && eq(probe_seq.pos) {
                return Ok(probe_seq.pos);
            }

            // If the tag is empty that means that the index couldn't be in subsequent elements
            // since if the tag was deleted there's a still a change the element could be
            // further down the line
            if tag.is_special() && tag.special_is_empty() {
                return Err(probe_seq.pos);
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    unsafe fn prepare_insert_slot(&mut self, hash: u64) -> usize {
        let index = self.find_insert_slot(hash);
        self.set_ctrl_hash(index, hash);
        index
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

    unsafe fn find_inner(&self, hash: u64, eq: &mut dyn FnMut(usize) -> bool) -> Option<usize> {
        debug_assert!(!self.is_empty());

        let tag_hash = Tag::full(hash);
        let mut probe_seq = self.probe_seq(hash);
        let ctrl_slice = self.ctrl_slice();

        loop {
            let tag = ctrl_slice.get(probe_seq.pos)?;

            if tag.eq(&tag_hash) && eq(probe_seq.pos) {
                return Some(probe_seq.pos);
            }

            if tag.is_special() && tag.special_is_empty() {
                return None;
            }

            probe_seq.move_next(self.bucket_mask);
        }
    }

    unsafe fn prepare_rehash_in_place(&mut self) {
        todo!()
    }

    // Returns a stack allocated object 
    fn prepare_resize<'a, A: Allocator>(
        &self,
        alloc: &'a A,
        table_layout: &TableLayout,
        capacity: usize,
    ) -> Result<ScopeGuard<Self, impl FnMut(&mut Self) + 'a>> {
        debug_assert!(self.items <= capacity);
        debug_assert!(capacity.is_power_of_two());

        let mut rt = RawTableInner::new();

        Ok(guard(rt, move |self_| {
            // I haven't figured out dropping quite yet
            // todo!()
        }))
    }

    unsafe fn reserve_rehash_inner<A: Allocator>(
        &mut self,
        alloc: &A,
        additional: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        table_layout: &TableLayout,
        _drop: Option<unsafe fn(*mut u8)>,
    ) -> Result<()> {
        let new_items = self.items + additional;
        let full_capacity = bucket_mask_to_capacity(self.bucket_mask);
        self.resize_inner(
            alloc,
            usize::max(new_items, full_capacity + 1),
            hasher,
            table_layout,
        )
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
        table_layout: &TableLayout,
    ) -> Result<()> {
        debug_assert!(self.items <= capacity);
        println!("desired capacity: {}", capacity);
        println!("pre_resize: {:?}", self);
        let buckets = capacity.next_power_of_two();

        let layout = table_layout.calculate_layout_for(buckets).unwrap().0;
        let global = unsafe { alloc.alloc(layout)? };

        let res_global = global.resolve_mut();
        let mut ref_slice: RefSliceMut<'_, Tag> = unsafe { RefSliceMut::from_ref(res_global.cast::<Tag>(), buckets) };
        ref_slice.fill_empty();

        let new_bucket_mask = buckets - 1;

        //let mut new_table = self.prepare_resize(alloc, table_layout, capacity)?;

        // Due to invariant pointers being unable to point to static objects, we're forced to allocate memory to reference things
        // therefore we need to check if the hashtable is null. Which is the case when the hashtable is empty. 
        // Also since invariant pointers require existing in an object, we can't take advantage of the RawTableInner functions
        // and thus have to duplicate their functionality but through a global pointer
        // TODO: Find a way better way to do this.
        if !self.is_empty() {
            println!("hi");
            for i in 0..self.buckets() {
                if self.ctrl_slice()[i].is_special() { continue; }

                let hash = hasher(self, i);
                // index =  find_insert_slot
                // prepare insert slot
                let index = 'resize_inner_exit: {
                    let mut probe_seq = LinearProbeSeq::new(hash, new_bucket_mask);

                    for i in 0..buckets {
                        let tag = ref_slice.get(probe_seq.pos).unwrap();
                        if tag.is_special() {
                            break 'resize_inner_exit probe_seq.pos;
                        }

                        probe_seq.move_next(self.bucket_mask);
                    }

                    panic!("how did we get here?");
                };
                ref_slice[index] = Tag::full(hash);

                let new_raw_ptr = ref_slice.as_mut_ptr().cast::<u8>().add(buckets).byte_add(index * table_layout.size);
                std::ptr::copy_nonoverlapping::<u8>(
                    self.bucket_ptr(i, table_layout),
                    new_raw_ptr,
                    table_layout.size,
                );
            }
        }

        // Since we are moving an invariant pointer, which depends on the object fot in which it's placed
        // we need to readd the FOT 
        /*let new_ctrl = new_table.ctrl.global();
        new_table.growth_left -= self.items;
        new_table.items = self.items;*/
        //mem::swap(self, &mut new_table);

        self.ctrl = InvPtr::new(Ref::from_ptr(self), global)?;
        self.bucket_mask = new_bucket_mask;
        self.growth_left = capacity - self.items;
        println!("post resize: {:?}", self);

        Ok(())
    }

    unsafe fn erase(&mut self, index: usize) {
        self.set_ctrl(index, Tag::DELETED);
        self.items -= 1;
    }

    unsafe fn bucket_ptr(&self, index: usize, table_layout: &TableLayout) -> *mut u8 {
        let data_offset = table_layout.calculate_data_offset(self.buckets());
        self.ctrl
            .resolve()
            .add(self.buckets() + index * table_layout.size)
            .as_mut()
            .raw()
    }

    fn bucket<T: Invariant>(&self, index: usize) -> Ref<T> {
        unsafe {
            self.ctrl
                .resolve()
                .add(self.buckets())
                .cast::<T>()
                .add(index)
        }
    }

    // Gets the pointer to the first bucket
    fn data_ref(&self, table_layout: &TableLayout) -> Ref<'_, u8> {
        let data_offset = table_layout.calculate_data_offset(self.buckets());
        unsafe { self.ctrl.resolve().add(self.buckets()) }
    }

    // Gets the pointer to the first bucket
    unsafe fn data_ref_mut(&self, table_layout: &TableLayout) -> RefMut<'_, u8> {
        let data_offset = table_layout.calculate_data_offset(self.buckets());
        self.ctrl.resolve().add(data_offset).as_mut()
    }

    unsafe fn set_ctrl_hash(&mut self, index: usize, hash: u64) {
        self.set_ctrl(index, Tag::full(hash));
    }

    unsafe fn set_ctrl(&mut self, index: usize, ctrl: Tag) {
        self.ctrl_slice_mut()[index] = ctrl;
    }

    unsafe fn ctrl_slice_mut(&mut self) -> RefSliceMut<'_, Tag> {
        let r = self.ctrl.resolve().cast::<Tag>().into_mut();
        let slice = RefSliceMut::from_ref(r, self.buckets());
        slice
    }

    unsafe fn ctrl_slice(&self) -> RefSlice<'_, Tag> {
        let r = self.ctrl.resolve().cast::<Tag>();
        let slice = RefSlice::from_ref(r, self.buckets());
        slice
    }
}
