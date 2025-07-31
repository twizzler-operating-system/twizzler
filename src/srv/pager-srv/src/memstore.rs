use std::sync::Arc;

use object_store::PagedObjectStore;
use twizzler::{object::ObjID, Result};
use twizzler_abi::{
    pager::{ObjectRange, PhysRange},
    syscall::{BackingType, LifetimeType},
};

use crate::helpers::PAGE;

pub mod virtio;
