/// An allocator that allocates MMIO addresses from a given range.

use core::alloc::Layout;

use crate::{memory::VirtAddr, spinlock::Spinlock};
use super::frame::FRAME_SIZE;

/// A simply bump allocator that does not reclaim memory.
/// This intended operating mode is okay for now. Addresses
/// are aligned up until the next page size.
pub struct BumpAlloc {
    // the start of this region
    start: VirtAddr,
    // the length of this region
    length: usize,
    // where in this region the next allocation takes place
    marker: VirtAddr,
}

impl BumpAlloc {
    const fn new(start: VirtAddr, length: usize) -> Self {
        BumpAlloc {
            start,
            length,
            marker: start,
        }
    }

    fn end(&self) -> usize {
        self.start.raw() as usize + self.length
    }

   pub fn alloc(&mut self, size: usize) -> Result<VirtAddr, ()> {
        // create a layout and allocate a range of addresses
        // based on an aligned allocation size
        let layout = Layout::from_size_align(size, FRAME_SIZE)
            .expect("failed to allocate region");
        // reserve space for this allocation size
        let new_marker = self.marker.raw() as usize + layout.size();
        if new_marker > self.end() {
            return Err(())
        } else {
            let va = self.marker;
            self.marker = VirtAddr::try_from(new_marker).map_err(|_| ())?;
            Ok(va)
        }
    }
}

pub static MMIO_ALLOCATOR: Spinlock<BumpAlloc> = Spinlock::new({
        let start = *VirtAddr::MMIO_RANGE.start();
        let end = *VirtAddr::MMIO_RANGE.end();
        let va_start = unsafe { VirtAddr::new_unchecked(start) };
        let length = end - start;
        BumpAlloc::new(va_start, length as usize)
    }
);
