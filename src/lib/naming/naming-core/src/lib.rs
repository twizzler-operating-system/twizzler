#![feature(linkage)]
use std::path::{Path, PathBuf};

pub mod api;
pub mod dynamic;
pub mod gates;
pub mod handle;
mod store;

// The generated `__twz_secgate_impl_*_mod` modules must be visible at the crate root: the
// server's `#[secgate::entry(lib = "naming-core")]` type-check names them by
// `naming_core::<mod>::Args`.
pub use gates::*;

pub const MAX_KEY_SIZE: usize = 256;
pub const PATH_MAX: usize = 4096;

/// Longest path that crosses a gate by value. Paths above this fall back to the shared buffer.
pub const INLINE_PATH_MAX: usize = 256;

/// One slot of a handle's shared buffer. Buffer-using calls carry a slot offset in their gate
/// arguments, so different slots can be in flight on one handle concurrently. A slot holds two
/// `PATH_MAX` paths (rename/link pack both into one slot) and bounds an enumerate reply.
pub const BUFFER_SLOT_SIZE: usize = 32768;
/// Slots per handle: the concurrency limit for buffer-using calls (inline calls are unlimited).
/// A caller that wants more waits for a slot, which is bounded by a gate call's duration.
pub const BUFFER_NSLOTS: usize = 8;

pub type Result<T> = std::result::Result<T, TwzError>;

pub use store::{
    cache_stats, memo_config, DevFs, GetFlags, NameSession, NameStore, NsNode, NsNodeKind,
};
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

    /// The path as it sits in the gate's own arguments.
    ///
    /// `to_path` allocates, and every caller that then hands the result to `NameSession::get` has
    /// it converted straight back to `&str` -- an allocation and a free per lookup to change type
    /// and nothing else. That is invisible single-threaded and is compartment-allocator contention
    /// at four.
    pub fn as_str(&self) -> Result<&str> {
        let len = (self.len as usize).min(INLINE_PATH_MAX);
        str::from_utf8(&self.bytes[..len])
            .map_err(|_| twizzler_rt_abi::error::ArgumentError::InvalidArgument.into())
    }

    pub fn to_path(&self) -> Result<PathBuf> {
        Ok(PathBuf::from(self.as_str()?))
    }
}

/// The reply to a working-directory query.
///
/// Carries the *full* length even when the path did not fit, so a caller can tell a short cwd
/// from a truncated one. [`InlinePath`] clamps its length on read, which would turn an over-long
/// cwd into its first [`INLINE_PATH_MAX`] bytes with nothing to say so.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct CwdPath {
    len: u32,
    bytes: [u8; INLINE_PATH_MAX],
}

impl CwdPath {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let bytes = path.as_ref().as_os_str().as_encoded_bytes();
        let mut this = Self {
            len: bytes.len() as u32,
            bytes: [0; INLINE_PATH_MAX],
        };
        let n = bytes.len().min(INLINE_PATH_MAX);
        this.bytes[..n].copy_from_slice(&bytes[..n]);
        this
    }

    /// Length of the cwd in bytes, whether or not it fit inline.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// `None` when the cwd was too long to inline; the caller re-asks through the buffer.
    pub fn as_str(&self) -> Option<Result<&str>> {
        if self.len() > INLINE_PATH_MAX {
            return None;
        }
        Some(
            str::from_utf8(&self.bytes[..self.len()])
                .map_err(|_| twizzler_rt_abi::error::ArgumentError::InvalidArgument.into()),
        )
    }
}
