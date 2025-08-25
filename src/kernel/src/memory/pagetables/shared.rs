use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{
    consistency::Consistency, MapReader, Mapper, MappingCursor, MappingSettings, PhysAddrProvider,
};
use crate::{
    arch::{memory::frame::FRAME_SIZE, PhysAddr, VirtAddr},
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        tracker::{alloc_frame, FrameAllocFlags},
    },
    mutex::Mutex,
};

struct SharedPageTableMgr {
    shared: Mutex<BTreeMap<PhysAddr, SharedPageTable>>,
}

impl SharedPageTableMgr {
    pub const fn new() -> Self {
        SharedPageTableMgr {
            shared: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, addr: PhysAddr, table: SharedPageTable) {
        self.shared.lock().insert(addr, table);
    }

    pub fn lookup(&self, addr: PhysAddr) -> Option<SharedPageTable> {
        self.shared.lock().get(&addr).cloned()
    }

    pub fn remove(&self, addr: PhysAddr) {
        self.shared.lock().remove(&addr);
    }
}

static SPT_MGR: SharedPageTableMgr = SharedPageTableMgr::new();

struct Inner {
    mapper: Mutex<Mapper>,
    refs: AtomicU64,
}

#[derive(Clone)]
pub struct SharedPageTable {
    inner: Arc<Inner>,
    pub settings: MappingSettings,
}

impl SharedPageTable {
    pub fn new(level: usize, settings: MappingSettings) -> Self {
        let root_page = alloc_frame(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
        )
        .start_address();
        let mut mapper = Mapper::new(root_page);
        mapper.set_start_level(level);
        let spt = SharedPageTable {
            inner: Arc::new(Inner {
                mapper: Mutex::new(mapper),
                refs: AtomicU64::new(1),
            }),
            settings,
        };
        SPT_MGR.insert(root_page, spt.clone());
        spt
    }

    pub fn inc_refs(&self) {
        self.inner.refs.fetch_add(1, Ordering::SeqCst);
    }

    pub fn dec_refs(&self) {
        let old = self.inner.refs.fetch_sub(1, Ordering::SeqCst);
        assert!(old > 0);
        if old == 1 {
            log::info!("no refs remaining for");
        }
    }

    pub fn level(&self) -> usize {
        self.inner.mapper.lock().start_level()
    }

    pub fn align_addr(&self, addr: VirtAddr) -> VirtAddr {
        VirtAddr::new(
            addr.raw()
                & !(PHYS_LEVEL_LAYOUTS[self.inner.mapper.lock().start_level()].size() - 1) as u64,
        )
        .unwrap()
    }

    pub fn provider(&self) -> SharedRootPageProvider<'_> {
        SharedRootPageProvider {
            done: false,
            shared: self,
        }
    }

    pub fn map(&self, cursor: MappingCursor, phys: &mut impl PhysAddrProvider) {
        #[cfg(target_arch = "x86_64")]
        let consist = Consistency::new_full_global();
        #[cfg(target_arch = "aarch64")]
        let consist = Consistency::new(todo!());
        let ops = self.inner.mapper.lock().map(cursor, phys, consist);
        if let Err(ops) = ops {
            log::warn!("failed to map in shared mapping: {:?}", cursor);
            ops.run_all();
        }
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        self.inner.mapper.lock().change(cursor, settings);
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = self.inner.mapper.lock().unmap(cursor);
        ops.run_all();
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = f(self.inner.mapper.lock().readmap(cursor));
        r
    }
}

pub struct SharedRootPageProvider<'a> {
    done: bool,
    shared: &'a SharedPageTable,
}

impl<'a> PhysAddrProvider for SharedRootPageProvider<'a> {
    fn peek(&mut self) -> Option<super::PhysMapInfo> {
        if !self.done {
            let addr = self.shared.inner.mapper.lock().root_address();
            Some(super::PhysMapInfo {
                addr,
                len: FRAME_SIZE,
                settings: self.shared.settings,
            })
        } else {
            None
        }
    }

    fn consume(&mut self, len: usize) {
        assert_eq!(len, FRAME_SIZE);
        self.done = true;
    }
}

pub fn free_shared_frame(frame: FrameRef) {
    let spt = SPT_MGR
        .lookup(frame.start_address())
        .expect("failed to find SharedPageTable from physical address");
    spt.dec_refs();
}
