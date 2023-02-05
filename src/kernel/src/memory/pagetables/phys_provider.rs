use crate::{
    arch::address::PhysAddr,
    memory::frame::{Frame, PhysicalFrameFlags},
};

#[derive(Debug, Clone, Copy)]
/// A contiguous region of physical memory for use in mapping.
pub struct PhysFrame {
    addr: PhysAddr,
    len: usize,
}

impl PhysFrame {
    /// Construct a new PhysFrame.
    pub fn new(addr: PhysAddr, len: usize) -> Self {
        Self { addr, len }
    }

    /// Get the start address of the frame.
    pub fn addr(&self) -> PhysAddr {
        self.addr
    }

    /// Get the length of the frame.
    pub fn len(&self) -> usize {
        self.len
    }
}

impl Into<PhysFrame> for Frame {
    fn into(self) -> PhysFrame {
        // TODO: This can be cleaned up once we merge Allen's work on addresses.
        PhysFrame {
            addr: self.start_address().as_u64().try_into().unwrap(),
            len: self.size(),
        }
    }
}

/// A trait for providing a set of physical pages to the mapping function.
pub trait PhysAddrProvider {
    /// Get the current physical frame.
    fn peek(&mut self) -> PhysFrame;
    /// Consume the current frame and go to the next one.
    fn consume(&mut self, len: usize);
}

/// An implementation of [PhysAddrProvider] that just allocates and returns freshly allocated and zeroed frames.
pub struct ZeroPageProvider {
    current: Option<PhysFrame>,
}

impl PhysAddrProvider for ZeroPageProvider {
    fn peek(&mut self) -> PhysFrame {
        match self.current {
            Some(frame) => frame,
            None => {
                let frame = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED).into();
                self.current = Some(frame);
                frame
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
            let x: u64 = f.addr().into();
            crate::memory::frame::free_frame(Frame::new(
                x86_64::PhysAddr::new(x),
                PhysicalFrameFlags::ZEROED,
            ));
        }
    }
}
