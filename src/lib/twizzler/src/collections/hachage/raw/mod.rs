use std::{
    alloc::{AllocError, Layout},
    marker::PhantomData,
    ptr::NonNull,
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use super::DefaultHashBuilder;
use crate::{
    alloc::Allocator,
    collections::hachage::control::{Tag, TagSliceExt},
    marker::{BaseType, Invariant},
    ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSliceMut},
    Result,
};

// todo figure out a better way to parameterize size
// generic feels too clunky, so does a const usize
#[derive(Default, Clone, Copy)]
pub struct HashTableAlloc {}

const MAX_ALIGNMENT: usize = 4096;
// We need some leeway for aligning the data
const MAX_SPACE: usize = MAX_SIZE - NULLPAGE_SIZE * 4 - MAX_ALIGNMENT;

// We need to have a set group width since invariant pointers mean that computers that use SSE2 or
// AVX512 can interpret the same hash table. 64 bytes is a good middle ground.
const INVARIANT_GROUP_WIDTH: usize = 64;

// This is a bit of a hack, but this assumes that the hash table is on a single object with no other
// allocations on the object. So we know the size of the tag, and the layout provides information
// on the data. However we do need to store the size, since Layout shows the total space of the
// allocation and not just the size of data. Also we can't just feed the Layout for the data array
// because we also want the hashtable to use other traditional allocators. Though this means this
// yields a pointer that overlaps with a previous allocation.

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
    // This will return a pointer to the middle of the allocation, specifically the ptr right after
    // the data portion or the first Tag
    fn allocate(
        &self,
        table_layout: &TableLayout,
        buckets: usize,
    ) -> std::result::Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        assert!(buckets.is_power_of_two());
        let (layout, _) = table_layout
            .calculate_layout_for(buckets)
            .ok_or(std::alloc::AllocError)?;
        if layout.align() > MAX_ALIGNMENT {
            return Err(std::alloc::AllocError);
        }
        // 1 for null page, 2 for metadata pages, 1 for base
        if layout.size() > MAX_SPACE {
            return Err(std::alloc::AllocError);
        }
        let offset = (MAX_SPACE / (table_layout.size + 1)) * table_layout.size;
        let corrected_offset = offset + (MAX_ALIGNMENT - offset % MAX_ALIGNMENT);
        let obj = twizzler_rt_abi::object::twz_rt_get_object_handle((self as *const Self).cast())
            .unwrap();

        Ok(GlobalPtr::new(
            obj.id(),
            (NULLPAGE_SIZE * 2 + corrected_offset) as u64,
        ))
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

// To reduce the number of Ref.resolves() to ideally one per call of RawTable
pub struct CarryCtx<'a> {
    backing: Ref<'a, u8>,
}

pub struct CarryCtxMut<'a> {
    backing: RefMut<'a, u8>,
}

pub trait Ctx {
    fn base(&self) -> *const u8;

    #[inline]
    fn ctrl<'a>(&self, buckets: usize) -> &'a [Tag] {
        unsafe { std::slice::from_raw_parts(self.base().cast::<Tag>(), buckets) }
    }

    #[inline]
    fn bucket<'a, T>(&self, index: usize) -> &'a T {
        unsafe { self.base().cast::<T>().sub(index + 1).as_ref_unchecked() }
    }
}

pub trait CtxMut: Ctx {
    fn base_mut(&self) -> *mut u8;

    #[inline]
    fn ctrl_mut(&self, buckets: usize) -> &mut [Tag] {
        unsafe { std::slice::from_raw_parts_mut(self.base_mut().cast(), buckets) }
    }

    #[inline]
    fn bucket_ptr(&self, index: usize, size_of: usize) -> *mut u8 {
        unsafe { self.base_mut().sub(size_of * (index + 1)) }
    }

    fn bucket_ref_mut<T>(&self, index: usize) -> &mut T {
        unsafe {
            self.base_mut()
                .cast::<T>()
                .sub(index + 1)
                .as_mut_unchecked()
        }
    }
}

impl CarryCtx<'_> {
    #[inline]
    pub fn new(base: Ref<u8>) -> CarryCtx<'_> {
        CarryCtx { backing: base }
    }
}

impl CarryCtxMut<'_> {
    #[inline]
    pub fn new(base: RefMut<u8>) -> CarryCtxMut<'_> {
        CarryCtxMut { backing: base }
    }
}

impl Ctx for CarryCtx<'_> {
    #[inline]
    fn base(&self) -> *const u8 {
        self.backing.raw()
    }
}

impl Ctx for CarryCtxMut<'_> {
    #[inline]
    fn base(&self) -> *const u8 {
        self.backing.raw().cast_const()
    }
}

impl CtxMut for CarryCtxMut<'_> {
    #[inline]
    fn base_mut(&self) -> *mut u8 {
        self.backing.raw()
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

    pub fn hasher(&self) -> &S {
        &self.hasher
    }

    pub fn allocator(&self) -> &A {
        &self.alloc
    }

    pub fn len(&self) -> usize {
        self.table.items
    }

    pub fn capacity(&self) -> usize {
        self.table.growth_left
    }

    pub fn with_hasher_in(hasher: S, alloc: A) -> Self {
        Self {
            table: RawTableInner::new(),
            hasher,
            alloc,
            _phantom: PhantomData,
        }
    }

    pub fn remove(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        ctx: &impl CtxMut,
    ) -> Option<T> {
        match unsafe {
            self.table
                .find_inner(hash, &mut |index| eq(&self.bucket(index)), ctx)
        } {
            Some(bucket) => Some(unsafe {
                let item = self.bucket(bucket).raw().read();
                self.table.erase(bucket);
                item
            }),
            None => None,
        }
    }

    pub fn get(&self, hash: u64, mut eq: impl FnMut(&T) -> bool, ctx: &impl Ctx) -> Option<&T> {
        unsafe {
            let result = self
                .table
                .find_inner(hash, &mut |index| eq(ctx.bucket::<T>(index)), ctx);

            match result {
                Some(index) => Some(ctx.bucket(index)),
                None => None,
            }
        }
    }

    pub fn get_mut<'a>(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        ctx: &'a impl CtxMut,
    ) -> Option<&'a mut T> {
        unsafe {
            let result = self
                .table
                .find_inner(hash, &mut |index| eq(ctx.bucket::<T>(index)), ctx);

            match result {
                Some(index) => Some(ctx.bucket_ref_mut(index)),
                None => None,
            }
        }
    }

    pub fn clear(&mut self) {
        if self.table.is_empty_singleton() {
            return;
        }
        let mut ctrl = unsafe { self.table.ctrl_slice_mut() };

        ctrl.fill_tag(Tag::EMPTY);
        self.table.items = 0;
    }

    fn bucket(&self, index: usize) -> Ref<T> {
        unsafe { self.table.bucket::<T>(index) }
    }

    pub fn carry_ctx(&self) -> CarryCtx {
        CarryCtx::new(unsafe { self.table.ctrl.resolve() })
    }

    pub fn carry_ctx_mut<'a>(&self, base: &RefMut<'a, RawTable<T, S, A>>) -> CarryCtxMut<'a> {
        CarryCtxMut::new(unsafe { base.table.ctrl.resolve_mut().owned() })
    }

    pub unsafe fn iter(&self) -> RawIter<T> {
        self.table.iter()
    }

    pub unsafe fn backing(&self) -> Ref<u8> {
        self.table.ctrl.resolve()
    }

    pub unsafe fn backing_mut(&mut self) -> RefMut<u8> {
        self.table.ctrl.resolve_mut()
    }
}

impl<T: Invariant, S> RawTable<T, S, HashTableAlloc> {
    pub fn bootstrap(&mut self, capacity: usize) -> Result<()> {
        unsafe {
            self.table
                .bootstrap(&self.alloc, capacity, &Self::TABLE_LAYOUT)
        }
    }

    pub fn reserve(&mut self, additional: usize, hasher: impl Fn(&T) -> u64, ctx: &impl CtxMut) {
        if core::intrinsics::unlikely(additional > self.table.growth_left) {
            self.reserve_rehash(additional, hasher, ctx).unwrap();
        }
    }

    pub fn reserve_rehash(
        &mut self,
        additional: usize,
        hasher: impl Fn(&T) -> u64,
        ctx: &impl CtxMut,
    ) -> Result<()> {
        unsafe {
            self.table.reserve_rehash_inner(
                &self.alloc,
                additional,
                &|_table, index| hasher(&ctx.bucket(index)),
                &Self::TABLE_LAYOUT,
                None,
                ctx,
            )
        }
    }

    pub unsafe fn resize(
        &mut self,
        capacity: usize,
        hasher: impl Fn(&T) -> u64,
        ctx: &impl CtxMut,
    ) -> Result<()> {
        self.table.resize_inner(
            &self.alloc,
            capacity,
            &|_table, index| hasher(ctx.bucket(index)),
            &Self::TABLE_LAYOUT,
            ctx,
        )
    }

    // Returns a reference to a slot or a candidate to insert
    pub fn find_or_find_insert_slot(
        &mut self,
        hash: u64,
        mut eq: impl FnMut(&T) -> bool,
        hasher: impl Fn(&T) -> u64,
        ctx: &impl CtxMut,
    ) -> std::result::Result<Ref<T>, usize> {
        // self.reserve() can change the invariant pointer (only when the hashtable is empty)
        self.reserve(1, hasher, ctx);

        unsafe {
            match self.table.find_or_find_insert_slot_inner(
                hash,
                &mut |index| eq(ctx.bucket(index)),
                ctx,
            ) {
                Ok(index) => Ok(self.bucket(index)),
                Err(slot) => Err(slot),
            }
        }
    }

    #[inline]
    pub unsafe fn insert_in_slot(&mut self, hash: u64, slot: usize, value: T, ctx: &impl CtxMut) {
        {
            let ctrl = ctx.ctrl_mut(self.table.buckets());
            let old_ctrl = ctrl[slot];
            self.table.growth_left -= usize::from(old_ctrl.special_is_empty());
            ctrl[slot] = Tag::full(hash);
            self.table.items += 1;
        }

        let bucket = ctx.bucket_ptr(slot, size_of::<T>()).cast::<T>();

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

    unsafe fn find_or_find_insert_slot_inner(
        &self,
        hash: u64,
        eq: &mut dyn FnMut(usize) -> bool,
        ctx: &impl Ctx,
    ) -> std::result::Result<usize, usize> {
        let tag_hash = Tag::full(hash);
        let mut probe_seq = self.probe_seq(hash);

        let ctrl = ctx.ctrl(self.buckets());
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

    unsafe fn find_insert_slot(&self, hash: u64, ctx: &impl Ctx) -> usize {
        let mut probe_seq = self.probe_seq(hash);
        let ctrl_slice = ctx.ctrl(self.buckets());
        loop {
            let tag = ctrl_slice[probe_seq.pos];
            if tag.is_special() {
                return probe_seq.pos;
            }
            probe_seq.move_next(self.bucket_mask);
        }
    }

    unsafe fn find_inner(
        &self,
        hash: u64,
        eq: &mut dyn FnMut(usize) -> bool,
        ctx: &impl Ctx,
    ) -> Option<usize> {
        debug_assert!(!self.is_empty());

        let tag_hash = Tag::full(hash);
        let mut probe_seq = self.probe_seq(hash);
        let ctrl_slice = ctx.ctrl(self.buckets());

        loop {
            let tag = ctrl_slice[probe_seq.pos];

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
        ctx: &impl CtxMut,
    ) -> Result<()> {
        let new_items = self.items + additional;
        let full_capacity = bucket_mask_to_capacity(self.bucket_mask);
        if new_items <= full_capacity / 2 {
            self.rehash_in_place(hasher, table_layout.size, drop, ctx);
        } else {
            self.resize_inner(
                alloc,
                usize::max(new_items, full_capacity + 1),
                hasher,
                table_layout,
                ctx,
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
        ctx: &impl CtxMut,
    ) {
        self.prepare_rehash_in_place(ctx);

        self.rehash_in_place_inner(hasher, size_of, self.buckets(), drop, ctx);
    }

    unsafe fn rehash_in_place_inner(
        &mut self,
        hasher: &dyn Fn(&mut Self, usize) -> u64,
        size_of: usize,
        workable_buckets: usize,
        _drop: Option<unsafe fn(*mut u8)>,
        ctx: &impl CtxMut,
    ) {
        // since this is also called by resize inner, in that case we don't need to work on all the
        // buckets just the area that may contain deleted entries.
        let ctrl = ctx.ctrl_mut(self.buckets());

        'outer: for i in 0..workable_buckets {
            if ctrl[i] != Tag::DELETED {
                continue;
            }

            let i_p = ctx.bucket_ptr(i, size_of);
            'inner: loop {
                let hash = hasher(self, i);

                let new_i = self.find_insert_slot(hash, ctx);

                if i == new_i {
                    self.replace_ctrl_hash(new_i, hash, ctx);
                    continue 'outer;
                }

                let new_i_p = ctx.bucket_ptr(new_i, size_of);

                let prev_ctrl = self.replace_ctrl_hash(new_i, hash, ctx);
                if prev_ctrl == Tag::EMPTY {
                    ctrl[i] = Tag::EMPTY;
                    std::ptr::copy_nonoverlapping(i_p, new_i_p, size_of);
                    continue 'outer;
                } else {
                    debug_assert_eq!(prev_ctrl, Tag::DELETED);
                    std::ptr::swap_nonoverlapping(i_p, new_i_p, size_of);
                    continue 'inner;
                }
            }
        }

        self.growth_left = bucket_mask_to_capacity(self.bucket_mask) - self.items;
    }

    unsafe fn prepare_rehash_in_place(&mut self, ctx: &impl CtxMut) {
        if self.is_empty() {
            return;
        }

        let slice = ctx.ctrl_mut(self.buckets());
        for i in 0..self.buckets() {
            if slice[i].is_full() {
                slice[i] = Tag::DELETED;
            } else {
                slice[i] = Tag::EMPTY;
            }
        }
    }

    unsafe fn bootstrap(
        &mut self,
        alloc: &HashTableAlloc,
        capacity: usize,
        table_layout: &TableLayout,
    ) -> Result<()> {
        let buckets = std::cmp::max((capacity * 8 / 7).next_power_of_two(), 8);

        let global = alloc.allocate(table_layout, buckets)?;
        self.ctrl = InvPtr::new(Ref::from_ptr(self), global)?;
        self.bucket_mask = buckets - 1;
        self.growth_left = bucket_mask_to_capacity(self.bucket_mask);
        self.items = 0;

        self.ctrl_slice_mut().fill_empty();

        Ok(())
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
        ctx: &impl CtxMut,
    ) -> Result<()> {
        debug_assert!(self.items <= capacity);

        let old_buckets = self.buckets();

        // To not waste work, we are preparing resize before we empty the tags to not operate on
        // tags we know are empty Allocate the tags in place, and empty them out
        let buckets = std::cmp::max((capacity * 8 / 7).next_power_of_two(), 8);
        let global = alloc.allocate(table_layout, buckets)?;
        if !self.is_empty_singleton() {
            self.prepare_rehash_in_place(ctx);
            let p = ctx.base_mut().add(self.buckets()).cast::<Tag>();
            let slice = std::slice::from_raw_parts_mut(p, buckets - self.buckets());
            slice.fill_empty();

            self.bucket_mask = buckets - 1;
            self.growth_left = bucket_mask_to_capacity(self.bucket_mask) - self.items;
            self.rehash_in_place_inner(hasher, table_layout.size, old_buckets, None, ctx);
        } else {
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

    unsafe fn bucket<T: Invariant>(&self, index: usize) -> Ref<T> {
        unsafe { self.ctrl.resolve().cast::<T>().sub(index + 1) }
    }

    #[inline]
    unsafe fn replace_ctrl_hash(&mut self, index: usize, hash: u64, ctx: &impl CtxMut) -> Tag {
        let ctrl = ctx.ctrl_mut(self.buckets());
        let prev_ctrl = ctrl[index];
        ctrl[index] = Tag::full(hash);
        prev_ctrl.to_owned()
    }

    #[inline]
    unsafe fn set_ctrl(&mut self, index: usize, ctrl: Tag) {
        self.ctrl_slice_mut()[index] = ctrl;
    }

    #[inline]
    unsafe fn ctrl_slice_mut(&mut self) -> RefSliceMut<'_, Tag> {
        let r = self.ctrl.resolve().cast::<Tag>().into_mut();
        let slice = RefSliceMut::from_ref(r, self.buckets());
        slice
    }

    // This function isn't responsible for guaranteeing mutability
    unsafe fn iter<T: Invariant>(&self) -> RawIter<T> {
        let data = self.ctrl.resolve().raw().cast::<T>().sub(1).cast_mut();
        RawIter::new(
            self.ctrl.resolve().raw().cast(),
            NonNull::new(data).unwrap(),
            self.buckets(),
        )
    }
}

// Assumes there exists a valid hashtable and object handle to it.
// also that the underlying hashtable doesn't change size
pub struct RawIter<T: Invariant> {
    data: NonNull<T>,
    ctrl: *const Tag,
    top: *const Tag,
}

impl<T: Invariant> RawIter<T> {
    pub(crate) unsafe fn new(ctrl: *const Tag, data: NonNull<T>, len: usize) -> RawIter<T> {
        let end = ctrl.add(len);

        Self {
            data,
            ctrl,
            top: end,
        }
    }

    pub(crate) unsafe fn next_impl(&mut self) -> Option<NonNull<T>> {
        loop {
            if self.ctrl >= self.top {
                return None;
            }

            if (*self.ctrl).is_full() {
                self.data = self.data.sub(1);
                self.ctrl = self.ctrl.add(1);
                return Some(self.data.add(1));
            }
            self.data = self.data.sub(1);
            self.ctrl = self.ctrl.add(1);
        }
    }
}

impl<T: Invariant> Iterator for RawIter<T> {
    type Item = NonNull<T>;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe { self.next_impl() }
    }
}
