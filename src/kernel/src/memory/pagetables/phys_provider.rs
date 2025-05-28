use super::MappingSettings;
use crate::{
    arch::address::PhysAddr,
    memory::{
        frame::FrameRef,
        tracker::{alloc_frame, free_frame, FrameAllocFlags},
    },
};

pub struct PhysMapInfo {
    pub addr: PhysAddr,
    pub len: usize,
    pub settings: MappingSettings,
}

/// A trait for providing a set of physical pages to the mapping function.
pub trait PhysAddrProvider {
    /// Get the current physical frame.
    fn peek(&mut self) -> Option<PhysMapInfo>;
    /// Consume the current frame and go to the next one.
    fn consume(&mut self, len: usize);
}

/// An implementation of [PhysAddrProvider] that just allocates and returns freshly allocated and
/// zeroed frames.
pub struct ZeroPageProvider {
    flags: FrameAllocFlags,
    settings: MappingSettings,
    current: Option<FrameRef>,
}

impl ZeroPageProvider {
    /// Create a new [ZeroPageProvider].
    pub fn new(flags: FrameAllocFlags, settings: MappingSettings) -> Self {
        Self {
            flags: flags | FrameAllocFlags::ZEROED,
            current: None,
            settings,
        }
    }
}

impl PhysAddrProvider for ZeroPageProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        match self.current {
            Some(frame) => Some(PhysMapInfo {
                addr: frame.start_address(),
                len: frame.size(),
                settings: self.settings,
            }),
            None => {
                let frame = alloc_frame(self.flags);
                self.current = Some(frame);
                Some(PhysMapInfo {
                    addr: frame.start_address(),
                    len: frame.size(),
                    settings: self.settings,
                })
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
    settings: MappingSettings,
}

impl ContiguousProvider {
    /// Construct a new [ContiguousProvider].
    pub fn new(start: PhysAddr, len: usize, settings: MappingSettings) -> Self {
        Self {
            next: start,
            rem: len,
            settings,
        }
    }
}

impl PhysAddrProvider for ContiguousProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        Some(PhysMapInfo {
            addr: self.next,
            len: self.rem,
            settings: self.settings,
        })
    }

    fn consume(&mut self, len: usize) {
        self.next = self.next.offset(len).unwrap();
        self.rem = self.rem.saturating_sub(len);
    }
}
