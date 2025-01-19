use std::{fs::OpenOptions, path::{Component, PathBuf}, sync::OnceLock};

use arrayvec::ArrayString;
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
pub mod store;

pub const MAX_KEY_SIZE: usize = 255;

