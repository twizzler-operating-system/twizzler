use crate::{
    arch::address::PhysAddr,
    memory::frame::{free_frame, FrameRef, PhysicalFrameFlags},
};

/// A trait for providing a set of physical pages to the mapping function.
pub trait PhysAddrProvider {
    /// Get the current physical frame.
    fn peek(&mut self) -> (PhysAddr, usize);
    /// Consume the current frame and go to the next one.
    fn consume(&mut self, len: usize);
}

#[derive(Default)]
/// An implementation of [PhysAddrProvider] that just allocates and returns freshly allocated and zeroed frames.
pub struct ZeroPageProvider {
    current: Option<FrameRef>,
}

impl PhysAddrProvider for ZeroPageProvider {
    fn peek(&mut self) -> (PhysAddr, usize) {
        match self.current {
            Some(frame) => (
                frame.start_address().as_u64().try_into().unwrap(),
                frame.size(),
            ),
            None => {
                let frame = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED).into();
                self.current = Some(frame);
                (
                    frame.start_address().as_u64().try_into().unwrap(),
                    frame.size(),
                )
            }
        }
    }

    fn consume(&mut self, _len: usize) {
        self.current = None;
    }
}

impl Drop for ZeroPageProvider {
    fn drop(&mut self) {
        // TODO: This can be cleaned up once we merge Allen's work on addresses.
        if let Some(f) = self.current.take() {
            free_frame(f);
        }
    }
}
