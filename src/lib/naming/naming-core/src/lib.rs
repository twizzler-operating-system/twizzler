use std::sync::OnceLock;

use arrayvec::ArrayString;
use monitor_api::CompartmentHandle;
use secgate::{
    util::{Descriptor, Handle, SimpleBuffer},
    DynamicSecGate, SecGateReturn,
};
use twizzler_rt_abi::object::{MapFlags, ObjID};

pub mod api;
pub mod definitions;
pub mod dynamic;
pub mod handle;

use handle::NamingHandle;
