/// Typical file system block size.
pub const BLOCK_SIZE: usize = 4096;
/// Typical system page size.
pub const PAGE_SIZE: usize = 4096;
/// Atomic sector size.
pub const SECTOR_SIZE: usize = 512;

/// 256-bit keys.
pub const KEY_SIZE: usize = 32;
/// 128-bit IVs.
pub const IV_LENGTH: usize = 16;
/// 64-bit versions (epoch IDs).
pub const VERSION_SIZE: usize = std::mem::size_of::<u64>();
/// Sector size without the version padding.
pub const UNPADDED_SECTOR_SIZE: usize = SECTOR_SIZE - VERSION_SIZE;
/// IO (read/write) size.
pub const IO_SIZE: usize = BLOCK_SIZE;

/// Key cache memory size limit.
pub const KEY_CACHE_LIMIT: usize = 4 * PAGE_SIZE;
/// Block key speculation chunk size.
pub const SPECULATION_CHUNK_SIZE: usize = 8192;
/// Buffer cache memory size limit.
pub const BUFFER_CACHE_SIZE: usize = 1 << 32;

/// The first epoch ID.
pub const EPOCH_ID_START: u64 = 0;

/// Toggle for concurrent IOs
pub const DISABLE_RW_CONCURRENCY: bool = false;
/// Enables instrumentation of time
pub const INSTRUMENT: bool = false;
