#![feature(io_error_more)]

use std::{fs::OpenOptions, path::{Component, PathBuf}, sync::OnceLock};

use monitor_api::CompartmentHandle;
use secgate::{
    util::{Descriptor, Handle, SimpleBuffer},
    DynamicSecGate, SecGateReturn,
};
use twizzler_rt_abi::object::{MapFlags, ObjID};
use twizzler::{
    alloc::invbox::InvBox, collections::vec::{Vec, VecObject, VecObjectAlloc}, marker::Invariant, object::{Object, ObjectBuilder, TypedObject}, ptr::{GlobalPtr, InvPtr}
};
use twizzler::ptr::Ref;
use std::path::Path;

pub mod api;
pub mod dynamic;
pub mod handle;
mod store;
mod error;

pub const MAX_KEY_SIZE: usize = 256;

pub use error::{Result, ErrorKind};
pub use store::{NameStore, NameSession, Entry, EntryType};
