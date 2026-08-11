use std::path::{Path, PathBuf};

pub mod api;
pub mod dynamic;
pub mod handle;
mod store;

pub const MAX_KEY_SIZE: usize = 256;
pub const PATH_MAX: usize = 4096;

/// Longest path that crosses a gate by value. Paths above this fall back to the shared buffer.
pub const INLINE_PATH_MAX: usize = 256;

pub type Result<T> = std::result::Result<T, TwzError>;

pub use store::{GetFlags, NameSession, NameStore, NsNode, NsNodeKind};
use twizzler_rt_abi::error::TwzError;

/// A path passed in a gate's arguments rather than through the handle's shared buffer.
///
/// The buffer is what forces a naming handle to be exclusively owned for the duration of a call --
/// it is written at offset 0 and read back by the server, so two callers sharing a handle would
/// read each other's paths. A lookup that carries its path inline touches no shared state, which is
/// what lets the hot path skip the buffer (and its object) entirely. Gate arguments are marshalled
/// onto the stack and need only `Copy`; `NsNode` already crosses by value at a comparable size.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct InlinePath {
    len: u32,
    bytes: [u8; INLINE_PATH_MAX],
}

impl InlinePath {
    /// `None` if the path is too long to inline; the caller falls back to the buffer.
    pub fn new(path: impl AsRef<Path>) -> Option<Self> {
        let bytes = path.as_ref().as_os_str().as_encoded_bytes();
        if bytes.len() > INLINE_PATH_MAX {
            return None;
        }
        let mut this = Self {
            len: bytes.len() as u32,
            bytes: [0; INLINE_PATH_MAX],
        };
        this.bytes[..bytes.len()].copy_from_slice(bytes);
        Some(this)
    }

    pub fn to_path(&self) -> Result<PathBuf> {
        let len = (self.len as usize).min(INLINE_PATH_MAX);
        let s = str::from_utf8(&self.bytes[..len])
            .map_err(|_| twizzler_rt_abi::error::ArgumentError::InvalidArgument)?;
        Ok(PathBuf::from(s))
    }
}
