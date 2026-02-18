#![feature(portable_simd)]
#![feature(lock_value_accessors)]

use std::sync::{Arc, Mutex, OnceLock};

use secgate::util::HandleMgr;
use virtio_net::{DeviceWrapper, TwizzlerTransport};

use crate::{client::Client, port::PortAssigner};

pub mod client;
pub mod device;
pub mod gates;
pub mod port;

static NETINFO: OnceLock<NetworkInfo> = OnceLock::new();
static PORTS: OnceLock<PortAssigner> = OnceLock::new();

#[allow(dead_code)]
struct NetworkInfo {
    handles: Mutex<HandleMgr<Arc<Client>>>,
    device: DeviceWrapper<TwizzlerTransport>,
}
