// arch specific frame (page) size in bytes.
// Frame size is chosen from translation granule.
// In this implementation we go with 4 KiB pages.
// In the future we could determine this at runtime.
pub const FRAME_SIZE: usize = 0x1000;
