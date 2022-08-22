use std::sync::{Arc, Mutex};

use twizzler_abi::{
    marker::BaseType,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{BackingType, LifetimeType},
};
use twizzler_object::{CreateSpec, Object};

use super::{Access, DeviceSync, DmaObject, DmaOptions, DmaRegion, DmaSliceRegion, DMA_PAGE_SIZE};

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
}

pub(super) struct AllocatableDmaObject {
    dma: DmaObject,
    freelist: Mutex<Vec<SplitPageRange>>,
}

/// A pool for allocating DMA regions that all share a common access type and DMA options.
pub struct DmaPool {
    opts: DmaOptions,
    spec: CreateSpec,
    access: Access,
    objects: Vec<Arc<AllocatableDmaObject>>,
}

/// Possible errors that can arise from a DMA pool allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllocationError {
    /// The requested region size was too large.
    TooBig,
    /// An internal error occurred.
    InternalError,
}

struct EmptyBase;

impl BaseType for EmptyBase {
    fn init<T>(_t: T) -> Self {
        Self
    }

    fn tags() -> &'static [(
        twizzler_abi::marker::BaseVersion,
        twizzler_abi::marker::BaseTag,
    )] {
        &[]
    }
}

impl AllocatableDmaObject {
    pub(super) fn dma_object(&self) -> &DmaObject {
        &self.dma
    }

    pub(super) fn free(&self, range: SplitPageRange) {
        let mut freelist = self.freelist.lock().unwrap();
        freelist.push(range);
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

    fn new(spec: &CreateSpec) -> Result<AllocatableDmaObject, AllocationError> {
        Ok(AllocatableDmaObject {
            // TODO: automatic object deletion.
            dma: DmaObject::new::<EmptyBase>(
                Object::create::<EmptyBase>(spec, EmptyBase)
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
    /// object is created using the provided [CreateSpec]. If default (volatile) options are
    /// acceptable for the create spec, use the [crate::dma::DmaPool::default_spec] function.
    pub fn new(spec: CreateSpec, access: Access, opts: DmaOptions) -> Self {
        Self {
            opts,
            spec,
            access,
            objects: vec![],
        }
    }

    /// Generate a default [CreateSpec] for use in creating Twizzler DMA objects. By default,
    /// Twizzler objects for DMA are placed in volatile memory with a volatile lifetime.
    pub fn default_spec() -> CreateSpec {
        CreateSpec::new(LifetimeType::Volatile, BackingType::Normal)
    }

    fn new_object(&mut self) -> Result<(), AllocationError> {
        let obj = Arc::new(AllocatableDmaObject::new(&self.spec)?);
        self.objects.push(obj);
        Ok(())
    }

    fn do_allocate(
        &mut self,
        len: usize,
    ) -> Result<(Arc<AllocatableDmaObject>, SplitPageRange), AllocationError> {
        if len > MAX_SIZE - NULLPAGE_SIZE * 2 {
            return Err(AllocationError::TooBig);
        }
        for obj in &self.objects {
            if let Some(pagerange) = obj.allocate(len) {
                return Ok((obj.clone(), pagerange));
            }
        }
        self.new_object()?;
        self.do_allocate(len)
    }

    /// Allocate a new [DmaRegion<T>] from the pool. The region will be initialized with the
    /// provided initial value.
    pub fn allocate<'a, T: DeviceSync>(
        &'a mut self,
        init: T,
    ) -> Result<DmaRegion<'a, T>, AllocationError> {
        let len = core::mem::size_of::<T>();
        let (ado, range) = self.do_allocate(len)?;
        let mut reg = DmaRegion::new(
            None,
            len,
            self.access,
            self.opts,
            range.offset(),
            Some((ado.clone(), range)),
        );
        reg.fill(init);
        Ok(reg)
    }

    /// Allocate a new [DmaSliceRegion<T>] from the pool. Each entry in the region's slice will
    /// be initialized with the provided initial value.
    pub fn allocate_array<'a, T: DeviceSync + Clone>(
        &'a mut self,
        count: usize,
        init: T,
    ) -> Result<DmaSliceRegion<'a, T>, AllocationError> {
        let len = core::mem::size_of::<T>() * count;
        let (ado, range) = self.do_allocate(len)?;
        let mut reg = DmaSliceRegion::new(
            None,
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

    /// Allocate a new [DmaSliceRegion<T>] from the pool. Each entry in the region's slice will
    /// be initialized by running the provided closure.
    pub fn allocate_array_with<'a, T: DeviceSync>(
        &'a mut self,
        count: usize,
        init: impl Fn() -> T,
    ) -> Result<DmaSliceRegion<'a, T>, AllocationError> {
        let len = core::mem::size_of::<T>() * count;
        let (ado, range) = self.do_allocate(len)?;
        let mut reg = DmaSliceRegion::new(
            None,
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
    use crate::dma::{Access, DmaOptions};

    use super::DmaPool;

    #[test]
    fn allocate() {
        let mut pool = DmaPool::new(
            DmaPool::default_spec(),
            Access::BiDirectional,
            DmaOptions::empty(),
        );

        let _res = pool.allocate(u32::MAX).unwrap();
    }
}
