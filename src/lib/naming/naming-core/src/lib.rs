use secgate::{util::{Descriptor, Handle, SimpleBuffer}, SecGateReturn};
use twizzler_rt_abi::object::{MapFlags, ObjID};
use arrayvec::ArrayString;
use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::DynamicSecGate;

pub mod handle;
pub mod api;
pub mod definitions;
pub mod dynamic;

use handle::NamingHandle;

