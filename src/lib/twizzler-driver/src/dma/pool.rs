use std::sync::{Arc, Mutex};

use twizzler::{
    marker::{BaseType, Invariant},
    object::ObjectBuilder,
};
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use super::{Access, DeviceSync, DmaObject, DmaOptions, DmaRegion, DmaSliceRegion, DMA_PAGE_SIZE};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub(super) struct SplitPageRange {
    start: usize,
    len: usize,
}

pub(super) enum Split {
    Single(SplitPageRange),
    Multiple(SplitPageRange, SplitPageRange),
}

impl SplitPageRange {
    fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }

    fn split(self, newlen: usize) -> Split {
        let start = self.start;
        let len = self.len;
        if newlen == 0 || newlen == len {
            return Split::Single(Self { start, len });
        }
        Split::Multiple(
            Self { start, len: newlen },
            Self {
                start: start + newlen,
                len: len - newlen,
            },
        )
    }

    fn merge(self, other: Self) -> Self {
        let (first, second) = if self.start < other.start {
            (self, other)
        } else {
            (other, self)
        };
        assert!(first.adjacent_before(&second));

        Self {
            start: first.start,
            len: first.len + second.len,
        }
    }

    fn adjacent_before(&self, other: &Self) -> bool {
        self.start < other.start && self.start + self.len == other.start
    }

    fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    fn start(&self) -> usize {
        self.start
    }

    fn offset(&self) -> usize {
        self.start * DMA_PAGE_SIZE
    }
}

#[cfg(test)]
pub mod tests_split_page_range {
    use super::SplitPageRange;
    use crate::dma::pool::compact_range_list;

    #[test]
    fn spr_split_multiple() {
        let r = SplitPageRange::new(2, 7);
        let split = r.split(4);
        if let super::Split::Multiple(a, b) = split {
            assert_eq!(a.len(), 4);
            assert_eq!(a.start(), 2);
            assert_eq!(b.len(), 3);
            assert_eq!(b.start(), 6);
        } else {
            panic!("split broken");
        }
    }

    #[test]
    fn spr_split_single1() {
        let r = SplitPageRange::new(2, 7);
        let split = r.split(7);
        if let super::Split::Single(r) = split {
            assert_eq!(r.len(), 7);
            assert_eq!(r.start(), 2);
        } else {
            panic!("split broken");
        }
    }

    #[test]
    fn spr_split_single2() {
        let r = SplitPageRange::new(2, 7);
        let split = r.split(0);
        if let super::Split::Single(r) = split {
            assert_eq!(r.len(), 7);
            assert_eq!(r.start(), 2);
        } else {
            panic!("split broken");
        }
    }

    #[test]
    fn spr_merge() {
        let a = SplitPageRange::new(2, 4);
        let b = SplitPageRange::new(6, 3);
        let r = a.merge(b);
        assert_eq!(r.start(), 2);
        assert_eq!(r.len(), 7);
    }

    #[test]
    fn spr_adj() {
        let a = SplitPageRange::new(2, 4);
        let b = SplitPageRange::new(1, 1);
        let c = SplitPageRange::new(6, 4);

        assert!(!a.adjacent_before(&b));
        assert!(b.adjacent_before(&a));
        assert!(!a.adjacent_before(&a));
        assert!(a.adjacent_before(&c));
    }

    #[test]
    fn spr_merge_alg() {
        let a = SplitPageRange::new(2, 4);
        let b = SplitPageRange::new(0, 1);
        let c = SplitPageRange::new(6, 4);
        let x = SplitPageRange::new(2, 8);
        let mut list = vec![a.clone(), b.clone(), c.clone()];
        let single_list = vec![a.clone()];
        let slw: Vec<_> = single_list.windows(2).collect();
        assert!(slw.is_empty());

        compact_range_list(&mut list);

        assert_eq!(list, vec![b, x]);
    }
}

pub(super) struct AllocatableDmaObject {
    dma: DmaObject,
    freelist: Mutex<Vec<SplitPageRange>>,
}

/// A pool for allocating DMA regions that all share a common access type and DMA options.
pub struct DmaPool {
    opts: DmaOptions,
    spec: ObjectBuilder<()>,
    access: Access,
    objects: Mutex<Vec<Arc<AllocatableDmaObject>>>,
}

/// Possible errors that can arise from a DMA pool allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllocationError {
    /// The requested region size was too large.
    TooBig,
    /// An internal error occurred.
    InternalError,
}

#[repr(C)]
struct EmptyBase;

unsafe impl Invariant for EmptyBase {}
impl BaseType for EmptyBase {}

// Merge adjacent regions by sorting, comparing pairs, and merging if they are adjacent.
// Keep going until we cannot merge anymore.
fn compact_range_list(list: &mut Vec<SplitPageRange>) {
    list.sort();
    loop {
        let pairs: Vec<_> = list
            .windows(2)
            .enumerate()
            .filter_map(|(idx, ranges)| {
                if ranges[0].adjacent_before(&ranges[1]) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if pairs.is_empty() {
            break;
        }

        // Iterate in reverse to compact from top, so as to not mess up indices.
        for pair in pairs.iter().rev() {
            // Grab the second item first to not mess up indices.
            let second = list.remove(pair + 1);
            let new = list[*pair].clone().merge(second);
            list[*pair] = new;
        }
    }
}

impl AllocatableDmaObject {
    pub(super) fn dma_object(&self) -> &DmaObject {
        &self.dma
    }

    pub(super) fn free(&self, range: SplitPageRange) {
        let mut freelist = self.freelist.lock().unwrap();
        freelist.push(range);

        compact_range_list(&mut freelist);
        // TODO: consider that, if the entire object get free'd, we could delete the object.
    }

    fn allocate(&self, len: usize) -> Option<SplitPageRange> {
        let mut freelist = self.freelist.lock().unwrap();
        let nr_pages = (len - 1) / DMA_PAGE_SIZE + 1;
        let index = freelist.iter().position(|range| range.len() >= nr_pages)?;

        let range = freelist.remove(index);
        Some(match range.split(nr_pages) {
            Split::Single(r) => r,
            Split::Multiple(alloc, extra) => {
                freelist.push(extra);
                alloc
            }
        })
    }

    fn new(spec: ObjectBuilder<()>) -> Result<AllocatableDmaObject, AllocationError> {
        Ok(AllocatableDmaObject {
            // TODO: automatic object deletion.
            dma: DmaObject::new::<EmptyBase>(
                spec.cast()
                    .build(EmptyBase)
                    .map_err(|_| AllocationError::InternalError)?,
            ),
            freelist: Mutex::new(vec![SplitPageRange::new(
                1,
                (MAX_SIZE - NULLPAGE_SIZE * 2) / DMA_PAGE_SIZE,
            )]),
        })
    }
}

impl DmaPool {
    /// Create a new DmaPool with access and DMA options, where each created underlying Twizzler
    /// object is created using the provided [ObjectBuilder]. If default (volatile) options are
    /// acceptable for the create spec, use the [crate::dma::DmaPool::default_spec] function.
    pub fn new(spec: ObjectBuilder<()>, access: Access, opts: DmaOptions) -> Self {
        Self {
            opts,
            spec,
            access,
            objects: Mutex::new(vec![]),
        }
    }

    pub fn default_spec() -> ObjectBuilder<()> {
        ObjectBuilder::default()
    }

    fn new_object(&self) -> Result<Arc<AllocatableDmaObject>, AllocationError> {
        let obj = Arc::new(AllocatableDmaObject::new(self.spec.clone())?);
        Ok(obj)
    }

    fn do_allocate(
        &self,
        len: usize,
    ) -> Result<(Arc<AllocatableDmaObject>, SplitPageRange), AllocationError> {
        if len > MAX_SIZE - NULLPAGE_SIZE * 2 {
            return Err(AllocationError::TooBig);
        }
        let mut objects = self.objects.lock().unwrap();
        for obj in &*objects {
            if let Some(pagerange) = obj.allocate(len) {
                return Ok((obj.clone(), pagerange));
            }
        }
        let obj = self.new_object()?;
        objects.push(obj);
        drop(objects);
        self.do_allocate(len)
    }

    /// Allocate a new `[DmaRegion]` from the pool. The region will be initialized with the
    /// provided initial value.
    pub fn allocate<'a, T: DeviceSync>(&'a self, init: T) -> Result<DmaRegion<T>, AllocationError> {
        let len = core::mem::size_of::<T>();
        let (ado, range) = self.do_allocate(len)?;
        let mut reg = DmaRegion::new(
            len,
            self.access,
            self.opts,
            range.offset(),
            Some((ado.clone(), range)),
        );
        reg.fill(init);
        Ok(reg)
    }

    /// Allocate a new `[DmaSliceRegion]` from the pool. Each entry in the region's slice will
    /// be initialized with the provided initial value.
    pub fn allocate_array<'a, T: DeviceSync + Clone>(
        &'a self,
        count: usize,
        init: T,
    ) -> Result<DmaSliceRegion<T>, AllocationError> {
        let len = core::mem::size_of::<T>() * count;
        let (ado, range) = self.do_allocate(len)?;
        let mut reg = DmaSliceRegion::new(
            len,
            self.access,
            self.opts,
            range.offset(),
            count,
            Some((ado.clone(), range)),
        );
        reg.fill(init);
        Ok(reg)
    }

    /// Allocate a new `[DmaSliceRegion]` from the pool. Each entry in the region's slice will
    /// be initialized by running the provided closure.
    pub fn allocate_array_with<'a, T: DeviceSync>(
        &'a self,
        count: usize,
        init: impl Fn() -> T,
    ) -> Result<DmaSliceRegion<T>, AllocationError> {
        let len = core::mem::size_of::<T>() * count;
        let (ado, range) = self.do_allocate(len)?;
        let mut reg = DmaSliceRegion::new(
            len,
            self.access,
            self.opts,
            range.offset(),
            count,
            Some((ado.clone(), range)),
        );
        reg.fill_with(init);
        Ok(reg)
    }
}

#[cfg(test)]
mod tests {
    use super::DmaPool;
    use crate::dma::{Access, DmaOptions};

    #[test]
    fn allocate() {
        let pool = DmaPool::new(
            DmaPool::default_spec(),
            Access::BiDirectional,
            DmaOptions::empty(),
        );

        let _res = pool.allocate(u32::MAX).unwrap();
    }
}
