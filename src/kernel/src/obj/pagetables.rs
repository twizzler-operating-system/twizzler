use alloc::sync::Arc;

use crate::{
    arch::VirtAddr,
    memory::{
        frame::FrameRef,
        pagetables::{Consistency, ContiguousProvider, Mapper, MappingCursor, MappingSettings},
        tracker::{FrameAllocFlags, alloc_frame},
    },
    mutex::Mutex,
};

pub struct ObjectPageTable {
    mapper: Arc<Mutex<Mapper>>,
}

impl ObjectPageTable {
    pub fn new() -> Self {
        let mapper = Mapper::new(
            alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
            )
            .start_address(),
        );
        Self {
            mapper: Arc::new(Mutex::new(mapper)),
        }
    }

    pub fn map_page(&self, offset: u64, page: FrameRef) -> bool {
        let consist = Consistency::new_full_global();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), page.size());
        let mut phys = ContiguousProvider::new(
            page.start_address(),
            page.size(),
            MappingSettings::default_user(),
        );
        if let Err(e) = self.mapper.lock().map(cursor, &mut phys, consist) {
            e.run_all();
            return false;
        }

        true
    }

    pub fn with_mapper<R>(&self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        let mut guard = self.mapper.lock();
        f(&mut guard)
    }
}
