use crate::{
    arch::address::PhysAddr,
    memory::frame::{alloc_frame, free_frame, FrameRef, PhysicalFrameFlags},
};

/// A trait for providing a set of physical pages to the mapping function.
pub trait PhysAddrProvider {
    /// Get the current physical frame.
    fn peek(&mut self) -> (PhysAddr, usize);
    /// Consume the current frame and go to the next one.
    fn consume(&mut self, len: usize);
}

#[derive(Default)]
/// An implementation of [PhysAddrProvider] that just allocates and returns freshly allocated and
/// zeroed frames.
pub struct ZeroPageProvider {
    current: Option<FrameRef>,
}

impl PhysAddrProvider for ZeroPageProvider {
    fn peek(&mut self) -> (PhysAddr, usize) {
        match self.current {
            Some(frame) => (frame.start_address(), frame.size()),
            None => {
                let frame = alloc_frame(PhysicalFrameFlags::ZEROED);
                self.current = Some(frame);
                (frame.start_address(), frame.size())
            }
        }
    }

    fn consume(&mut self, _len: usize) {
        self.current = None;
    }
}

impl Drop for ZeroPageProvider {
    fn drop(&mut self) {
        if let Some(f) = self.current.take() {
            free_frame(f);
        }
    }
}

/// Implements [PhysAddrProvider] by providing physical addresses within a given range.
pub struct ContiguousProvider {
    next: PhysAddr,
    rem: usize,
}

impl ContiguousProvider {
    /// Construct a new [ContiguousProvider].
    pub fn new(start: PhysAddr, len: usize) -> Self {
        Self {
            next: start,
            rem: len,
        }
    }
}

impl PhysAddrProvider for ContiguousProvider {
    fn peek(&mut self) -> (PhysAddr, usize) {
        (self.next, self.rem)
    }

    fn consume(&mut self, len: usize) {
        self.next = self.next.offset(len).unwrap();
        self.rem = self.rem.saturating_sub(len);
    }
}
