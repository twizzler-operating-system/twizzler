use std::{
    alloc::{AllocError, Layout},
    marker::PhantomData,
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use super::DefaultHashBuilder;
use crate::{
    alloc::Allocator,
    collections::hachage::control::*,
    marker::{BaseType, Invariant},
    ptr::{GlobalPtr, InvPtr, Ref, RefSlice, RefSliceMut},
    Result,
};

// todo figure out a better way to parameterize size
// generic feels too clunky, so does a const usize
#[derive(Default, Clone, Copy)]
pub struct HashTableAlloc {}

const MAX_ALIGNMENT: usize = 4096;
// We need some leeway for aligning the data
const MAX_SPACE: usize = MAX_SIZE - NULLPAGE_SIZE * 4 - MAX_ALIGNMENT;

// We need to have a set group width since invariant pointers mean that computers that use SSE2 or AVX512 can interpret
// the same hash table. 64 bytes is a good middle ground.
const INVARIANT_GROUP_WIDTH: usize = 64;

// This is a bit of a hack, but this assumes that the hash table is on a single object with no other 
// allocations on the object. So we know the size of the tag, and the layout provides information
// on the data. However we do need to store the size, since Layout shows the total space of the allocation
// and not just the size of data. Also we can't just feed the Layout for the data array because we also
// want the hashtable to use other traditional allocators. Though this means this yields a pointer
// that overlaps with a previous allocation. 

impl Allocator for HashTableAlloc {
    fn alloc(
        &self,
        _layout: Layout,
    ) -> std::result::Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        panic!("unsupported")
    }

    unsafe fn dealloc(&self, _ptr: crate::ptr::GlobalPtr<u8>, _layout: Layout) {}

    unsafe fn realloc(
        &self,
        _ptr: GlobalPtr<u8>,
        _layout: Layout,
        _newsize: usize,
    ) -> std::result::Result<GlobalPtr<u8>, AllocError> {
        panic!("unsupported")
    }
}

impl HashTableAlloc {
    // This will return a pointer to the middle of the allocation, specifically the ptr right after the data portion or the first Tag 
    fn allocate(
        &self,
        table_layout: &TableLayout,
        buckets: usize
    ) -> std::result::Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        assert!(buckets.is_power_of_two());
        let (layout, _) = table_layout.calculate_layout_for(buckets).ok_or(std::alloc::AllocError)?;
        if layout.align() > MAX_ALIGNMENT { return Err(std::alloc::AllocError)}
        // 1 for null page, 2 for metadata pages, 1 for base
        if layout.size() > MAX_SPACE {
            return Err(std::alloc::AllocError);
        }
        let offset = (MAX_SPACE / (table_layout.size + 1)) * table_layout.size; 
        let corrected_offset = offset + (MAX_ALIGNMENT - offset % MAX_ALIGNMENT);
        let obj = twizzler_rt_abi::object::twz_rt_get_object_handle((self as *const Self).cast()).unwrap();

        Ok(GlobalPtr::new(obj.id(), (NULLPAGE_SIZE * 2 + corrected_offset) as u64))
    }
}

#[inline]
#[allow(clippy::cast_possible_truncation)]
fn h1(hash: u64) -> usize {
    // On 32-bit platforms we simply ignore the higher hash bits.
    hash as usize
}

#[derive(Copy, Clone)]
struct TableLayout {
    pub size: usize,
    pub ctrl_align: usize,
}

impl TableLayout {
    #[inline]
    const fn new<T: Invariant>() -> Self {
        let layout = Layout::new::<T>();
        Self {
            size: layout.size(),
            ctrl_align: if layout.align() > INVARIANT_GROUP_WIDTH {
                layout.align()
            } else {
                INVARIANT_GROUP_WIDTH
            },
        }
    }

    fn calculate_ctrl_offset(&self, buckets: usize) -> usize {
        self.size * buckets + (self.ctrl_align - 1) & !(self.ctrl_align - 1)
    }

    #[inline]
    fn calculate_layout_for(&self, buckets: usize) -> Option<(Layout, usize)> {
        debug_assert!(buckets.is_power_of_two());

        // TODO: fix this for regular allocators and do bounds checking
        // maybe enforce a minimum element count? 
        // Well I guess we can just check it twice maybe.
        // This calculates the nearest alignment value for buckets * self.size 
        let ctrl_offset = self.calculate_ctrl_offset(buckets);
        let len = ctrl_offset + buckets + INVARIANT_GROUP_WIDTH;

        Some((
            unsafe { Layout::from_size_align_unchecked(len, self.ctrl_align) },
            ctrl_offset,
        ))
    }
}

struct ProbeSeq {
    pos: usize,
}

impl ProbeSeq {
    fn move_next(&mut self, bucket_mask: usize) {
        self.pos += 1;
        self.pos &= bucket_mask
    }
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

    pub fn len(&self) -> usize {
        self.table.items
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

    pub fn remove(&mut self, hash: u64, mut eq: impl FnMut(&T) -> bool) -> Option<T> {
        match unsafe { self.table.find_inner(hash, &mut |index| eq(&self.bucket(index))) } {
            Some(bucket) => Some(
                unsafe { 
                    let item = self.bucket(bucket).raw().read();
                    self.table.erase(bucket);
                    item
                }

            ),
            None => None,
        }
    }

    pub fn get(&self, hash: u64, eq: impl FnMut(&T) -> bool) -> Option<&T> {
        match self.find(hash, eq) {
            Some(r) => unsafe { r.raw().as_ref() },
            None => None,
        }
    }

    fn bucket(&self, index: usize) -> Ref<T> {
        unsafe { self.table.bucket::<T>(index) }
    }

    pub unsafe fn replace_hasher(&mut self, _hasher: S) -> Result<()> {
        todo!()
    }
}

impl<T: Invariant, S> RawTable<T, S, HashTableAlloc> {
    pub fn reserve(&mut self, additional: usize, hasher: impl Fn(&T) -> u64) {
        if additional > self.table.growth_left {
            self.reserve_rehash(additional, hasher).unwrap()
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

    // replace the hasher, changing the hashing state and essentially shuffling the values

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
    fn probe_seq(&self, hash: u64) -> ProbeSeq {
        ProbeSeq {
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

    unsafe fn reserve_rehash_inner(
        &mut self,
        alloc: &HashTableAlloc,
        additional: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        table_layout: &TableLayout,
        drop: Option<unsafe fn(*mut u8)>,
    ) -> Result<()> {
        let new_items = self.items + additional;
        let full_capacity = bucket_mask_to_capacity(self.bucket_mask);
        if new_items <= full_capacity / 2 {
            self.rehash_in_place(hasher, table_layout.size, drop);
        } else {
            self.resize_inner(
                alloc,
                usize::max(new_items, full_capacity + 1),
                hasher,
                table_layout,
            )?
        }

        Ok(())
    }

    // to avoid unnecessary work between rehash_in_place and resize_inner
    unsafe fn rehash_in_place(
        &mut self,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        size_of: usize,
        drop: Option<unsafe fn(*mut u8)>, 
    ) {
        self.prepare_rehash_in_place();

        self.rehash_in_place_inner(hasher, size_of, self.buckets(), drop);
    }

    unsafe fn rehash_in_place_inner(
        &mut self,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        size_of: usize,
        workable_buckets: usize,
        _drop: Option<unsafe fn(*mut u8)>,
    ) {        
        //let instant = std::time::Instant::now();
        
        // since this is also called by resize inner, in that case we don't need to work on all the buckets just the area 
        // that may contain deleted entries.
        'outer: for i in 0..workable_buckets {
            if *self.ctrl(i) != Tag::DELETED {
                continue;
            }

            let i_p = self.bucket_ptr(i, size_of);
            'inner: loop {
                let hash = hasher(self, i);
    
                let new_i = self.find_insert_slot(hash);

                if i == new_i {
                    self.replace_ctrl_hash(new_i, hash);
                    continue 'outer;
                }

                let new_i_p = self.bucket_ptr(new_i, size_of);
                
                let prev_ctrl = self.replace_ctrl_hash(new_i, hash);
                if prev_ctrl == Tag::EMPTY {
                    self.set_ctrl(i, Tag::EMPTY);
                    std::ptr::copy_nonoverlapping(i_p, new_i_p, size_of);
                    continue 'outer;
                }
                else {
                    debug_assert_eq!(prev_ctrl, Tag::DELETED);
                    std::ptr::swap_nonoverlapping(i_p, new_i_p, size_of);
                    continue 'inner;
                }
            }
        }
        //println!("rehash in place took {}", instant.elapsed().as_millis());
        self.growth_left = bucket_mask_to_capacity(self.bucket_mask) - self.items;
    }

    unsafe fn prepare_rehash_in_place(&mut self) {
        if self.is_empty() {return;}

        let mut slice = RefSliceMut::<Tag>::from_ref(self.ctrl.resolve().cast::<Tag>().into_mut(), self.buckets());
        for i in 0..self.buckets() {
            if slice[i].is_full() {
                slice[i] = Tag::DELETED;
            }
            else {
                slice[i] = Tag::EMPTY;
            }
        }
    }

    // I'm using alloc instead of realloc here, making an assumption about how the allocator works
    // but this gets me half the potential capacity of an object I need to do a rehash in place
    // instead but with two different sizes on the same array?
    // But once I create a sharding wrapper the amount a single object holds matters less.
    // TODO: Redo this once a real allocation function is made
    unsafe fn resize_inner(
        &mut self,
        alloc: &HashTableAlloc,
        capacity: usize,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        table_layout: &TableLayout,
    ) -> Result<()> {
        debug_assert!(self.items <= capacity);

        let old_buckets = self.buckets();

        // To not waste work, we are preparing resize before we empty the tags to not operate on tags we know are empty
        
        // Allocate the tags in place, and empty them out
        let buckets = std::cmp::max((capacity* 8 / 7).next_power_of_two() , 8);
        let global = alloc.allocate(table_layout, buckets)?;
        if !self.is_empty_singleton() {
            self.prepare_rehash_in_place();
            {
                let r = self.ctrl.resolve();
                let newly_allocated_range = r.add(self.buckets()).cast::<Tag>().into_mut();
                let mut slice = RefSliceMut::from_ref(newly_allocated_range, buckets - self.buckets());
                slice.fill_empty();
            }

            self.bucket_mask = buckets - 1;
            self.growth_left = bucket_mask_to_capacity(self.bucket_mask) - self.items;
            self.rehash_in_place_inner(hasher, table_layout.size,  old_buckets, None);
        }
        else {
            self.ctrl = InvPtr::new(Ref::from_ptr(self), global)?;
            self.bucket_mask = buckets - 1;
            self.growth_left = bucket_mask_to_capacity(self.bucket_mask);
            self.items = 0;

            self.ctrl_slice_mut().fill_empty();
        }

        Ok(())
    }

    unsafe fn erase(&mut self, index: usize) {
        self.set_ctrl(index, Tag::DELETED);
        self.items -= 1;
    }

    unsafe fn bucket_ptr(&self, index: usize, layout_size: usize) -> *mut u8 {
        let base: *mut u8 = self.ctrl.resolve().sub((index + 1) * layout_size).as_mut().raw();
        base
    }

    unsafe fn bucket<T: Invariant>(&self, index: usize) -> Ref<T> {
        unsafe {
            self.ctrl
                .resolve()
                .cast::<T>()
                .sub(index + 1)
        }
    }

    unsafe fn replace_ctrl_hash(&mut self, index: usize, hash: u64) -> Tag {
        let prev_ctrl = *self.ctrl(index);
        self.set_ctrl_hash(index, hash);
        prev_ctrl
    }

    unsafe fn ctrl(&self, index: usize) -> *mut Tag {
        self.ctrl.resolve().add(index).into_mut().raw().cast()
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
