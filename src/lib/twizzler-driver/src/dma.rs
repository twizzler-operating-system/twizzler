use std::{
    alloc::Layout,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use twizzler_abi::{
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
    marker::BaseType,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_kaction, BackingType, LifetimeType},
};
use twizzler_object::{CreateSpec, Object};

struct DmaBase;

impl BaseType for DmaBase {
    fn init<T>(_t: T) -> Self {
        Self
    }
    fn tags() -> &'static [(
        twizzler_abi::marker::BaseVersion,
        twizzler_abi::marker::BaseTag,
    )] {
        todo!()
    }
}

struct DmaObjectInner {
    object: Object<DmaBase>,
    huge_regions: Vec<usize>,
    page_regions: Vec<usize>,
    small_offsets: Vec<(usize, usize)>,
    map_huge: BTreeMap<usize, u64>,
    map_page: BTreeMap<usize, u64>,
}

// TODO
const HUGE_SZ: usize = 2 * 1024 * 1024;
const PAGE_SZ: usize = 4096;

impl DmaObjectInner {
    pub fn new() -> Option<Self> {
        let spec = CreateSpec::new(LifetimeType::Volatile, BackingType::Normal);
        let object = Object::create(&spec, ()).ok()?;
        let mut off = NULLPAGE_SIZE;
        let end = MAX_SIZE - NULLPAGE_SIZE;
        let mut huge_regions = Vec::new();
        let mut page_regions = Vec::new();
        while off < end {
            let rem = end - off;
            if rem >= HUGE_SZ && off % HUGE_SZ == 0 {
                huge_regions.push(off);
            } else {
                page_regions.push(off);
            }
            off += NULLPAGE_SIZE;
        }
        Some(Self {
            object,
            huge_regions,
            page_regions,
            small_offsets: Vec::new(),
            map_huge: BTreeMap::new(),
            map_page: BTreeMap::new(),
        })
    }

    fn free(&mut self, loc: usize, len: usize) {
        if len == HUGE_SZ {
            self.huge_regions.push(loc);
        } else if len == PAGE_SZ {
            self.page_regions.push(loc)
        } else {
            self.small_offsets.push((loc, len));
        }
        // TODO: release back to OS?
    }

    fn do_kernel_backing(&self, loc: usize, len: usize) -> Option<u64> {
        sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::AllocateDMA(
                (len / PAGE_SZ).try_into().unwrap(),
            )),
            Some(self.object.id()),
            loc as u64,
            KactionFlags::empty(),
        )
        .ok()?
        .u64()
    }

    fn do_backing(&mut self, loc: usize, len: usize) -> bool {
        let res = self.do_kernel_backing(loc, len);
        if res.is_none() {
            return false;
        }
        let backing: u64 = res.unwrap();
        if len == HUGE_SZ {
            assert_eq!(loc % HUGE_SZ, 0);
            assert_eq!(backing % HUGE_SZ as u64, 0);
            self.map_huge.insert(loc, backing);
        } else if len == PAGE_SZ {
            assert_eq!(loc % PAGE_SZ, 0);
            assert_eq!(backing % PAGE_SZ as u64, 0);
            self.map_page.insert(loc, backing);
        } else {
            panic!("cannot back a size other than page or huge");
        }
        true
    }

    fn split_huge_to_pages(&mut self, reg: usize) {
        for i in 0..(HUGE_SZ / PAGE_SZ) {
            self.page_regions.push(reg + i * PAGE_SZ);
        }
    }

    fn split_huge(&mut self, reg: usize, size: usize) {
        if size >= HUGE_SZ {
            return;
        }
        self.small_offsets.push((reg + size, HUGE_SZ - size));
    }

    fn split_page(&mut self, reg: usize, size: usize) {
        if size >= PAGE_SZ {
            return;
        }
        self.small_offsets.push((reg + size, PAGE_SZ - size));
    }

    fn alloc_huge(&mut self) -> Option<usize> {
        let reg = self.huge_regions.pop()?;
        if !self.do_backing(reg, HUGE_SZ) {
            return None;
        }
        Some(reg)
    }

    fn alloc_page(&mut self) -> Option<usize> {
        if self.page_regions.is_empty() {
            let hr = self.huge_regions.pop()?;
            self.split_huge_to_pages(hr);
        }
        let reg = self.huge_regions.pop()?;
        if !self.do_backing(reg, PAGE_SZ) {
            return None;
        }
        Some(reg)
    }

    fn try_small(&mut self, size: usize) -> Option<usize> {
        let idx = self
            .small_offsets
            .iter()
            .position(|(_, len)| *len >= size)?;
        let (reg, l) = self.small_offsets.remove(idx);
        let new_len = l - size;
        let new_reg = reg + size;
        self.small_offsets.push((new_reg, new_len));
        Some(reg)
    }

    pub fn allocate(&mut self, size: usize) -> Option<usize> {
        if size > HUGE_SZ {
            return None;
        }
        if let Some(reg) = self.try_small(size) {
            return Some(reg);
        }
        if size > PAGE_SZ {
            let reg = self.alloc_huge()?;
            self.split_huge(reg, size);
            return Some(reg);
        }
        let reg = self.alloc_page()?;
        self.split_page(reg, size);
        Some(reg)
    }

    pub fn lookup_phys(&self, reg: usize) -> u64 {
        let huge_start = reg - (reg % HUGE_SZ);
        let page_start = reg - (reg % PAGE_SZ);
        if let Some(x) = self.map_huge.get(&huge_start) {
            return *x + (reg % HUGE_SZ) as u64;
        }
        if let Some(x) = self.map_page.get(&page_start) {
            return *x + (reg % PAGE_SZ) as u64;
        }
        panic!("unknown physical mapping");
    }

    pub fn to_virt(&self, reg: usize) -> *mut u8 {
        self.object.raw_lea_mut(reg)
    }
}

struct DmaObject {
    inner: Mutex<DmaObjectInner>,
}
pub struct DmaAllocator {
    objs: Mutex<Vec<Arc<DmaObject>>>,
}

pub struct DmaRegion<'a> {
    virt: *mut u8,
    phys: u64,
    layout: Layout,
    obj: Arc<DmaObject>,
    loc: usize,
    allocator: &'a DmaAllocator,
}

pub enum DmaAllocationError {
    InvalidLayout,
    AllocationFailed,
}

impl DmaAllocator {
    pub(crate) fn new() -> Self {
        Self {
            objs: Mutex::new(Vec::new()),
        }
    }

    pub fn allocate(&self, layout: Layout) -> Result<DmaRegion<'_>, DmaAllocationError> {
        let layout = Layout::from_size_align(layout.size(), layout.align())
            .map_err(|_| DmaAllocationError::InvalidLayout)?;
        if layout.size() > HUGE_SZ {
            return Err(DmaAllocationError::InvalidLayout);
        }
        for _ in 0..2 {
            let mut objs = self.objs.lock().unwrap();
            for obj in objs.iter() {
                let mut inner = obj.inner.lock().unwrap();
                if let Some(x) = inner.allocate(layout.size()) {
                    return Ok(DmaRegion {
                        virt: inner.to_virt(x),
                        phys: inner.lookup_phys(x),
                        layout,
                        obj: obj.clone(),
                        loc: x,
                        allocator: self,
                    });
                }
            }
            objs.push(Arc::new(DmaObject {
                inner: Mutex::new(
                    DmaObjectInner::new().ok_or(DmaAllocationError::AllocationFailed)?,
                ),
            }));
        }
        return Err(DmaAllocationError::AllocationFailed);
    }

    fn free(&self, reg: &DmaRegion) {
        reg.obj
            .inner
            .lock()
            .unwrap()
            .free(reg.loc, reg.layout.size());
    }
}

impl<'a> Drop for DmaRegion<'a> {
    fn drop(&mut self) {
        self.allocator.free(self);
    }
}

impl<'a> DmaRegion<'a> {
    pub unsafe fn as_ref<T>(&self) -> &T {
        (self.virt as *const T).as_ref().unwrap_unchecked()
    }

    pub unsafe fn as_mut<T>(&mut self) -> &mut T {
        (self.virt as *mut T).as_mut().unwrap_unchecked()
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }
}
