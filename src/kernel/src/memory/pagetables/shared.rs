use alloc::sync::Arc;

use super::{MapReader, Mapper, MappingCursor, MappingSettings, PhysAddrProvider};
use crate::{
    arch::{memory::frame::FRAME_SIZE, VirtAddr},
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        tracker::{alloc_frame, FrameAllocFlags},
    },
    mutex::Mutex,
};

#[derive(Clone)]
pub struct SharedPageTable {
    mapper: Arc<Mutex<Mapper>>,
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
        SharedPageTable {
            mapper: Arc::new(Mutex::new(mapper)),
            settings,
        }
    }

    pub fn level(&self) -> usize {
        self.mapper.lock().start_level()
    }

    pub fn align_addr(&self, addr: VirtAddr) -> VirtAddr {
        VirtAddr::new(
            addr.raw() & !(PHYS_LEVEL_LAYOUTS[self.mapper.lock().start_level()].size() - 1) as u64,
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
        let ops = self.mapper.lock().map(cursor, phys);
        if let Err(ops) = ops {
            ops.run_all();
        }
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        self.mapper.lock().change(cursor, settings);
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = self.mapper.lock().unmap(cursor);
        ops.run_all();
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = f(self.mapper.lock().readmap(cursor));
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
            let addr = self.shared.mapper.lock().root_address();
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
    todo!()
}
