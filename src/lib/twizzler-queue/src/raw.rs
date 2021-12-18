use std::sync::atomic::AtomicU64;

pub(crate) struct RawQueue<'a> {
    entries: *mut u8,
    length: usize,
    stride: usize,
    ctrl_word: &'a AtomicU64,
}
