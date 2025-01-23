#![feature(io_error_more)]

pub mod api;
pub mod dynamic;
pub mod handle;
mod store;
mod error;

pub const MAX_KEY_SIZE: usize = 256;

pub use error::{Result, ErrorKind};
pub use store::{NameStore, NameSession, Entry, EntryType};
