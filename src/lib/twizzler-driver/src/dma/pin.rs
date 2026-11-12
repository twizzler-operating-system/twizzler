use std::ops::Index;

use crate::arch::DMA_PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
/// A physical address. Must be aligned on [DMA_PAGE_SIZE].
pub struct PhysAddr(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Information about a page of DMA memory, including it's physical address.
pub struct PhysInfo {
    addr: PhysAddr,
}

/// An iterator over DMA memory pages, returning [PhysInfo].
pub struct DmaPinIter<'a> {
    pin: &'a [PhysInfo],
    idx: usize,
}

/// A representation of some pinned memory for a region.
pub struct DmaPin<'a> {
    pub backing: &'a [PhysInfo],
}

impl<'a> DmaPin<'a> {
    pub(super) fn new(backing: &'a [PhysInfo]) -> Self {
        Self { backing }
    }

    pub fn len(&self) -> usize {
        self.backing.len()
    }
}

impl PhysInfo {
    pub fn new(addr: PhysAddr) -> Self {
        Self { addr }
    }

    /// Get the address of this DMA memory page.
    pub fn addr(&self) -> PhysAddr {
        self.addr
    }
}

impl From<PhysInfo> for u64 {
    fn from(p: PhysInfo) -> Self {
        p.addr.into()
    }
}

impl From<&PhysInfo> for u64 {
    fn from(p: &PhysInfo) -> Self {
        p.addr.into()
    }
}

impl From<PhysAddr> for u64 {
    fn from(p: PhysAddr) -> Self {
        p.0
    }
}

impl TryFrom<u64> for PhysAddr {
    type Error = ();

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value & (DMA_PAGE_SIZE as u64 - 1) != 0 {
            return Err(());
        }
        Ok(Self(value))
    }
}

impl<'a> IntoIterator for DmaPin<'a> {
    type Item = PhysInfo;

    type IntoIter = DmaPinIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        DmaPinIter {
            pin: self.backing,
            idx: 0,
        }
    }
}

impl<'a> Iterator for DmaPinIter<'a> {
    type Item = PhysInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.pin.len() {
            None
        } else {
            let ret = self.pin[self.idx];
            self.idx += 1;
            Some(ret)
        }
    }
}

impl<'a> Index<usize> for DmaPin<'a> {
    type Output = PhysInfo;

    fn index(&self, index: usize) -> &Self::Output {
        &self.backing[index]
    }
}

#[cfg(test)]
mod tests {
    use twizzler::object::{ObjID, ObjectBuilder, RawObject};
    use twizzler_abi::{
        kso::{
            pack_kaction_pin_start_and_len, KactionCmd, KactionFlags, KactionGenericCmd, PinExtent,
            PinFlags, PinRequest,
        },
        object::{Protections, NULLPAGE_SIZE},
        syscall::{
            sys_kaction, sys_object_copy, sys_object_create, BackingType, LifetimeType,
            ObjectCreate, ObjectCreateFlags, PinnedPage,
        },
    };
    use twizzler_rt_abi::bindings::object_source;

    use crate::dma::{Access, DmaObject, DmaOptions, DmaPool, DMA_PAGE_SIZE};

    fn release(id: ObjID, token: u32) -> bool {
        sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::ReleasePin),
            Some(id),
            token as u64,
            0,
            KactionFlags::empty(),
        )
        .is_ok()
    }

    /// Released pins are gone, and pinning again finds the same frames.
    #[test]
    fn pin_range_release_and_repin() {
        let obj = fresh(false);
        let id = obj.object().id();
        let pin = obj.pin_range(PAGE..LARGE, PinFlags::empty()).unwrap();
        let ext = pin.extents().to_vec();
        let mut out = [PinExtent::default(); 64];
        let req = raw_pin_range(id, PAGE, LARGE - PAGE, &mut out);
        assert_ne!(req.token, 0);
        unsafe { pin.release() };
        assert!(release(id, req.token));
        assert!(!release(id, req.token));
        let again = obj.pin_range(PAGE..LARGE, PinFlags::empty()).unwrap();
        assert_eq!(again.extents(), ext);
    }

    /// Zeroing a pinned range, or copy-on-write from it, is refused until the pin is released.
    #[test]
    fn pin_range_refuses_replacement() {
        let obj = fresh(false);
        let id = obj.object().id();
        let pin = obj
            .pin_range(LARGE..LARGE + 4 * PAGE, PinFlags::empty())
            .unwrap();
        let zero = object_source {
            id: 0,
            src_start: 0,
            dest_start: LARGE as u64,
            len: PAGE as u64,
        };
        let from = object_source {
            id: id.raw(),
            src_start: LARGE as u64,
            dest_start: LARGE as u64,
            len: 4 * PAGE as u64,
        };
        let create = || {
            sys_object_create(
                ObjectCreate::new(
                    BackingType::Normal,
                    LifetimeType::Volatile,
                    None,
                    ObjectCreateFlags::DELETE,
                    Protections::all(),
                ),
                &[from],
                &[],
            )
        };
        assert!(sys_object_copy(id, &[zero]).is_err());
        assert!(create().is_err());
        unsafe { pin.release() };
        sys_object_copy(id, &[zero]).unwrap();
        create().unwrap();
    }

    const LARGE: usize = 2 * 1024 * 1024;
    const PAGE: usize = DMA_PAGE_SIZE;

    fn fresh(persist: bool) -> DmaObject {
        DmaObject::new(
            ObjectBuilder::<()>::default()
                .persist(persist)
                .build(())
                .unwrap(),
        )
    }

    fn raw_pin_range(id: ObjID, offset: usize, len: usize, out: &mut [PinExtent]) -> PinRequest {
        let mut req = PinRequest::new(offset as u64, len as u64, out);
        sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::PinRange(0)),
            Some(id),
            &mut req as *mut PinRequest as u64,
            0,
            KactionFlags::empty(),
        )
        .unwrap();
        req
    }

    fn raw_pin_pages(id: ObjID, offset: usize, len: usize) -> Vec<u64> {
        let mut pins = vec![PinnedPage::new(0); len / PAGE];
        sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::PinPages(0)),
            Some(id),
            pins.as_mut_ptr() as u64,
            pack_kaction_pin_start_and_len((offset / PAGE) as u64, len / PAGE).unwrap(),
            KactionFlags::empty(),
        )
        .unwrap();
        pins.iter().map(|p| p.physical_address()).collect()
    }

    fn pages(extents: &[PinExtent]) -> Vec<u64> {
        extents
            .iter()
            .flat_map(|e| (0..e.len).step_by(PAGE).map(move |o| e.phys + o))
            .collect()
    }

    /// The physical address of byte `offset` of a range pinned from `start`.
    fn phys_at(extents: &[PinExtent], start: usize, offset: usize) -> u64 {
        pages(extents)[(offset - start) / PAGE]
    }

    fn check_merged(extents: &[PinExtent]) {
        for pair in extents.windows(2) {
            assert_ne!(pair[0].phys + pair[0].len, pair[1].phys);
        }
        assert!(extents
            .iter()
            .all(|e| e.len > 0 && e.len % PAGE as u64 == 0 && e.phys % PAGE as u64 == 0));
    }

    fn fill(obj: &DmaObject, offset: usize, len: usize, seed: u64) {
        let base = unsafe { obj.object().base_mut_ptr::<u8>().sub(NULLPAGE_SIZE) };
        for o in (offset..offset + len).step_by(PAGE) {
            unsafe { (base.add(o) as *mut u64).write_volatile(o as u64 ^ seed) };
        }
    }

    fn check(obj: &DmaObject, offset: usize, len: usize, seed: u64) {
        let base = unsafe { obj.object().base_mut_ptr::<u8>().sub(NULLPAGE_SIZE) };
        for o in (offset..offset + len).step_by(PAGE) {
            assert_eq!(
                unsafe { (base.add(o) as *const u64).read_volatile() },
                o as u64 ^ seed
            );
        }
    }

    /// [4 KiB, 8 MiB) of a fresh object: every 2 MiB boundary lands on a 2 MiB-aligned physical
    /// address, the extents are maximal, and the old per-page call agrees.
    #[test]
    fn pin_range_fresh_shape() {
        let obj = fresh(false);
        let range = PAGE..4 * LARGE;
        let pin = obj.pin_range(range.clone(), PinFlags::empty()).unwrap();
        let ext = pin.extents();
        assert_eq!(ext.iter().map(|e| e.len).sum::<u64>(), range.len() as u64);
        check_merged(ext);
        for region in 1..4 {
            assert_eq!(phys_at(ext, PAGE, region * LARGE) % LARGE as u64, 0);
            // A large frame is one physical run over the whole region.
            assert_eq!(
                phys_at(ext, PAGE, region * LARGE + LARGE - PAGE),
                phys_at(ext, PAGE, region * LARGE) + (LARGE - PAGE) as u64
            );
        }
        assert_eq!(
            raw_pin_pages(obj.object().id(), range.start, range.len()),
            pages(ext)
        );
        fill(&obj, range.start, range.len(), 0x77);
        check(&obj, range.start, range.len(), 0x77);
        let again = obj.pin_range(range.clone(), PinFlags::empty()).unwrap();
        assert_eq!(again.extents(), ext);
    }

    /// A one-extent buffer pins a prefix; continuing from `covered` reaches the same extents.
    #[test]
    fn pin_range_small_buffer_continues() {
        let obj = fresh(false);
        let range = PAGE..LARGE + 64 * PAGE;
        // Populated piecewise first, so the range is many small runs.
        fill(&obj, range.start, range.len(), 1);
        let all = obj.pin_range(range.clone(), PinFlags::empty()).unwrap();
        let mut joined = Vec::new();
        let mut at = range.start;
        while at < range.end {
            let mut out = [PinExtent::default(); 1];
            let req = raw_pin_range(obj.object().id(), at, range.end - at, &mut out);
            assert_eq!(req.nr, 1);
            assert!(req.covered > 0 && req.covered as usize <= range.end - at);
            assert_eq!(out[0].len, req.covered);
            joined.push(out[0]);
            at += req.covered as usize;
        }
        assert_eq!(joined, all.extents());
        check(&obj, range.start, range.len(), 1);
    }

    /// CONTIGUOUS through the extent call: one run, and a whole aligned region is one large frame.
    #[test]
    fn pin_range_contiguous() {
        let obj = fresh(false);
        let pin = obj.pin_range(PAGE..7 * PAGE, PinFlags::CONTIGUOUS).unwrap();
        assert_eq!(pin.extents().len(), 1);
        let pin = obj
            .pin_range(LARGE..2 * LARGE, PinFlags::CONTIGUOUS)
            .unwrap();
        assert_eq!(pin.extents().len(), 1);
        assert_eq!(pin.extents()[0].phys % LARGE as u64, 0);
        fill(&obj, LARGE, LARGE, 3);
        check(&obj, LARGE, LARGE, 3);
    }

    /// Pinning a pager-backed range brings its pages in.
    #[test]
    fn pin_range_pager_backed() {
        let obj = fresh(true);
        let range = PAGE..PAGE + 32 * PAGE;
        fill(&obj, range.start, 4 * PAGE, 9);
        let pin = obj.pin_range(range.clone(), PinFlags::empty()).unwrap();
        assert_eq!(
            pin.extents().iter().map(|e| e.len).sum::<u64>(),
            range.len() as u64
        );
        check_merged(pin.extents());
        check(&obj, range.start, 4 * PAGE, 9);
        fill(&obj, range.start, range.len(), 10);
        check(&obj, range.start, range.len(), 10);
    }

    #[test]
    fn pin_kaction() {
        let pool = DmaPool::new(
            DmaPool::default_spec(),
            Access::BiDirectional,
            DmaOptions::default(),
        );
        let mut reg = pool.allocate(0u32).unwrap();
        let pin = reg.pin();
        let pin = pin.unwrap();
        for phys in pin {
            let addr = phys.addr();
            let addr: u64 = addr.into();
            assert!(addr & 0xfff == 0);
            assert_ne!(addr, 0);
        }
    }
}
