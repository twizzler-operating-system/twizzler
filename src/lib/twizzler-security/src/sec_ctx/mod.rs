use alloc::collections::btree_map::BTreeMap;
use core::fmt::Display;

use base::{CtxMapItemType, InsertType, SecCtxBase, SecCtxFlags};
use log::debug;
use twizzler::object::{Object, ObjectBuilder, RawObject, TypedObject};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::ObjectCreate,
};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

use crate::{Cap, VerifyingKey};

mod base;
pub use base::*;

#[cfg(feature = "user")]
mod user;
#[cfg(feature = "user")]
pub use user::*;

// pub mod map;
/// Information about protections for a given object within a context.
#[derive(Clone, Copy)]
pub struct PermsInfo {
    ctx: ObjID,
    prot: Protections,
}
