pub mod api;
pub mod dynamic;
pub mod handle;
mod store;

pub const MAX_KEY_SIZE: usize = 256;
pub const PATH_MAX: usize = 4096;

pub type Result<T> = std::result::Result<T, TwzError>;

pub use store::{GetFlags, NameSession, NameStore, NsNode, NsNodeKind};
use twizzler_rt_abi::error::TwzError;
