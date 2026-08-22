use super::MappingSettings;
use crate::{
    arch::address::PhysAddr,
    memory::{
        frame::FrameRef,
        tracker::{FrameAllocFlags, alloc_frame, free_frame},
    },
};

#[derive(Debug)]
pub struct PhysMapInfo {
    pub addr: PhysAddr,
    pub len: usize,
    pub settings: MappingSettings,
    /// The frame at `addr`, when the provider already holds it.
    ///
    /// `Table::map` takes a reference on the frame it installs and finds it with
    /// `get_frame(paddr.addr)` -- a linear scan over every frame indexer followed by a load from
    /// the frame array. Every provider that allocates a frame, or is handed one, already has the
    /// answer and discards it reducing itself to an address.
    ///
    /// **Only set this when `addr` is the frame's own start address**, because that is the only
    /// case where it equals what `get_frame(addr)` would have returned: the frame array is indexed
    /// per 4 KiB, so an offer taken mid-way into a larger frame resolves to a different `Frame`.
    /// `None` means "look it up", which is exactly the previous behaviour.
    pub frame: Option<FrameRef>,
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
        Self::with_flags(flags | FrameAllocFlags::ZEROED, settings)
    }

    fn with_flags(flags: FrameAllocFlags, settings: MappingSettings) -> Self {
        Self {
            flags,
            current: None,
            settings,
        }
    }
}

/// An implementation of [PhysAddrProvider] that returns freshly allocated frames whose contents are
/// **undefined**.
///
/// For mappings whose consumer writes before it reads. The kernel heap is the case that matters:
/// growing it mapped zeroed frames, which for a fresh region means touching every page of it, and
/// a never-touched frame is cold in the host as well -- one 8 MiB growth measured at 13 ms. Nothing
/// downstream needs the zeroes: `GlobalAllocWrapper` has no `alloc_zeroed` of its own, so it uses
/// the default `alloc` + `write_bytes`, ferroc's base declares `IS_ZEROED = false`, and the kernel
/// stack allocator explicitly does not want them.
///
/// Note this does mean kernel heap pages arrive holding whatever their frames last held, which can
/// include bytes from a userspace object's pages. Eager zeroing used to mask that; any kernel
/// structure copied to userspace without being fully initialized is now a leak.
pub struct UninitPageProvider(ZeroPageProvider);

impl UninitPageProvider {
    pub fn new(flags: FrameAllocFlags, settings: MappingSettings) -> Self {
        Self(ZeroPageProvider::with_flags(
            flags & !FrameAllocFlags::ZEROED,
            settings,
        ))
    }
}

impl PhysAddrProvider for UninitPageProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        self.0.peek()
    }

    fn consume(&mut self, len: usize) {
        self.0.consume(len)
    }
}

impl PhysAddrProvider for ZeroPageProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        match self.current {
            Some(frame) => Some(PhysMapInfo {
                addr: frame.start_address(),
                len: frame.size(),
                settings: self.settings,
                frame: Some(frame),
            }),
            None => {
                let frame = alloc_frame(self.flags);
                self.current = Some(frame);
                Some(PhysMapInfo {
                    addr: frame.start_address(),
                    len: frame.size(),
                    settings: self.settings,
                    frame: Some(frame),
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

/// Offers a run of separately-allocated frames, one frame per offer.
///
/// Each offer is capped at that frame's own size, for the reason
/// [`ContiguousProvider::new_of_page_size`] gives: `Table::can_map_at` tests the length the
/// provider *offers*, so a run offered whole becomes one huge entry holding a single refcount over
/// memory owned by many frames. Here the cap is structural rather than a parameter -- `peek`
/// cannot offer more than one frame, because it does not know the next one is adjacent.
///
/// `Table::map` consumes an offer for an entry it finds **already present** without mapping it, so
/// the caller must have established that every offset in the run is absent -- otherwise a frame is
/// silently dropped. [`Self::consumed`] is what a partial failure aborts the tail from.
pub struct FrameSliceProvider<'a> {
    frames: &'a [FrameRef],
    idx: usize,
    settings: MappingSettings,
}

impl<'a> FrameSliceProvider<'a> {
    pub fn new(frames: &'a [FrameRef], settings: MappingSettings) -> Self {
        Self {
            frames,
            idx: 0,
            settings,
        }
    }

    /// How many offers `Table::map` took. The tail is untouched and still the caller's.
    pub fn consumed(&self) -> usize {
        self.idx
    }
}

impl PhysAddrProvider for FrameSliceProvider<'_> {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        let frame = self.frames.get(self.idx)?;
        Some(PhysMapInfo {
            addr: frame.start_address(),
            len: frame.size(),
            settings: self.settings,
            frame: Some(*frame),
        })
    }

    fn consume(&mut self, _len: usize) {
        self.idx += 1;
    }
}

/// Implements [PhysAddrProvider] by providing physical addresses within a given range.
pub struct ContiguousProvider {
    next: Option<PhysAddr>,
    rem: usize,
    max_peek: usize,
    settings: MappingSettings,
}

impl ContiguousProvider {
    /// Construct a new [ContiguousProvider]. The range is offered whole, so [Table::map] may cover
    /// it with large pages where alignment allows.
    pub fn new(start: PhysAddr, len: usize, settings: MappingSettings) -> Self {
        Self {
            next: Some(start),
            rem: len,
            max_peek: usize::MAX,
            settings,
        }
    }

    /// A range that must be mapped one `page_size` entry at a time.
    ///
    /// `Table::can_map_at` tests the length the provider *offers* against the level's page size, so
    /// a range offered whole becomes a huge page as soon as both addresses are aligned to one. That
    /// is right for a single frame of that size and wrong for a run of separate smaller frames:
    /// `Table::map` takes a reference on the frame at the entry's physical address and no others,
    /// so the huge entry would hold one refcount over memory owned by many frames -- and unmapping
    /// it would free exactly one of them. Capping what is offered keeps the walk at the leaf level
    /// while still doing the whole run in one descent.
    pub fn new_of_page_size(
        start: PhysAddr,
        len: usize,
        page_size: usize,
        settings: MappingSettings,
    ) -> Self {
        Self {
            next: Some(start),
            rem: len,
            max_peek: page_size,
            settings,
        }
    }
}

impl PhysAddrProvider for ContiguousProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        Some(PhysMapInfo {
            addr: self.next?,
            len: self.rem.min(self.max_peek),
            settings: self.settings,
            // A raw physical range: there may be no `Frame` behind it at all (MMIO), so this
            // provider cannot answer and `Table::map` falls back to the lookup.
            frame: None,
        })
    }

    fn consume(&mut self, len: usize) {
        if let Some(next) = &mut self.next {
            if let Some(n) = next.offset(len).ok() {
                *next = n;
                self.rem = self.rem.saturating_sub(len);
            }
        }
    }
}
