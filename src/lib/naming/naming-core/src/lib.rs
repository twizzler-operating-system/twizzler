#![feature(io_error_more)]

pub mod api;
pub mod dynamic;
mod error;
pub mod handle;
mod store;

pub const MAX_KEY_SIZE: usize = 256;

pub use error::{ErrorKind, Result};
pub use store::{Entry, EntryType, NameSession, NameStore};
