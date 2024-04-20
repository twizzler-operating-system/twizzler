/// The architechture specific frame (page) size in bytes.
///
/// Frame size is chosen from translation granule.
/// In this implementation we go with 4 KiB pages.
pub const FRAME_SIZE: usize = FrameSize::Size4KiB as usize;

/// The possible frame sizes for a page of memory
/// mapped by a page table.
enum FrameSize {
    Size4KiB = 1 << 12,
    Size16KiB = 1 << 14,
    Size64KiB = 1 << 16,
}
