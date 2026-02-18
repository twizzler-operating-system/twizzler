use std::{
    net::IpAddr,
    str::FromStr,
    sync::{atomic::Ordering, Mutex},
};

use secgate::{util::HandleMgr, ResourceError, TwzError};
use smoltcp::wire::EthernetAddress;
use tracing::Level;
use twizzler::{object::RawObject, Result};
use twizzler_abi::syscall::ObjectCreate;
use twizzler_net::{
    packet::PacketObject, ClientMsg, ClientRet, NetClientConfig, NetClientOpenInfo, NetServer,
    ServerMsg, ServerRet,
};

use crate::{
    client::Client, device::device_thread, port::PortAssigner, NetworkInfo, NETINFO, PORTS,
};

const IP: &str = "10.0.2.15"; // QEMU user networking default IP
const GATEWAY: &str = "10.0.2.2"; // QEMU user networking gateway

#[secgate::entry(lib = "twizzler-net")]
pub fn start_network() -> Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    if NETINFO.get().is_some() {
        tracing::info!("cannot call start_network more than once");
        return Err(TwzError::NOT_SUPPORTED);
    }

    let device = virtio_net::get_device();
    let _device = device.clone();
    std::thread::spawn(move || device_thread(_device));
    tracing::info!("network ready: IP = {}, gateway = {}", IP, GATEWAY);

    let _ = PORTS.set(PortAssigner::new());

    let _ = NETINFO.set(NetworkInfo {
        handles: Mutex::new(HandleMgr::new(None)),
        device,
    });

    Ok(())
}

#[secgate::entry(lib = "twizzler-net")]
fn twz_net_alloc_port(desc: secgate::util::Descriptor, port: Option<u16>) -> Result<u16> {
    let handles = NETINFO
        .get()
        .ok_or(TwzError::NOT_SUPPORTED)?
        .handles
        .lock()
        .unwrap();
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let caller = info.source_context().ok_or(TwzError::INVALID_ARGUMENT)?;
    let client = handles
        .lookup(caller, desc)
        .ok_or(TwzError::INVALID_ARGUMENT)?;

    let port = if let Some(port) = port {
        if PORTS.get().unwrap().allocate_port(port) {
            Some(port)
        } else {
            None
        }
    } else {
        PORTS.get().unwrap().get_ephemeral_port()
    };
    let Some(port) = port else {
        return Err(ResourceError::OutOfResources.into());
    };

    client.ports.lock().unwrap().insert(port);
    Ok(port)
}

#[secgate::entry(lib = "twizzler-net")]
fn twz_net_release_port(desc: secgate::util::Descriptor, port: u16) -> Result<()> {
    let handles = NETINFO
        .get()
        .ok_or(TwzError::NOT_SUPPORTED)?
        .handles
        .lock()
        .unwrap();
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let caller = info.source_context().ok_or(TwzError::INVALID_ARGUMENT)?;
    let client = handles
        .lookup(caller, desc)
        .ok_or(TwzError::INVALID_ARGUMENT)?;

    if client.ports.lock().unwrap().remove(&port) {
        PORTS.get().unwrap().return_port(port);
        Ok(())
    } else {
        Err(TwzError::INVALID_ARGUMENT)
    }
}

#[secgate::entry(lib = "twizzler-net")]
fn twz_net_drop_client(desc: secgate::util::Descriptor) -> Result<()> {
    let mut handles = NETINFO
        .get()
        .ok_or(TwzError::NOT_SUPPORTED)?
        .handles
        .lock()
        .unwrap();
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let caller = info.source_context().ok_or(TwzError::INVALID_ARGUMENT)?;
    if let Some(client) = handles.remove(caller, desc) {
        client.active.store(false, Ordering::SeqCst);
        for port in client.ports.lock().unwrap().drain() {
            PORTS.get().unwrap().return_port(port);
        }
    }
    Ok(())
}

#[secgate::entry(lib = "twizzler-net")]
pub fn twz_net_open_client(_config: NetClientConfig) -> Result<NetClientOpenInfo> {
    let mut handles = NETINFO
        .get()
        .ok_or(TwzError::NOT_SUPPORTED)?
        .handles
        .lock()
        .unwrap();

    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let caller = info.source_context().ok_or(TwzError::INVALID_ARGUMENT)?;

    let tx_buf = PacketObject::new(ObjectCreate::default(), 1024, 2048)?;
    let rx_buf = PacketObject::new(ObjectCreate::default(), 1024, 2048)?;

    let rx_queue_obj = unsafe {
        twizzler::object::ObjectBuilder::<()>::default()
            .build_ctor(|obj| {
                twizzler_queue::Queue::<ServerMsg, ClientRet>::init(obj.handle(), 1024, 1024)
            })
            .expect("failed to create queue")
    };
    let tx_queue_obj = unsafe {
        twizzler::object::ObjectBuilder::<()>::default()
            .build_ctor(|obj| {
                twizzler_queue::Queue::<ClientMsg, ServerRet>::init(obj.handle(), 1024, 1024)
            })
            .expect("failed to create queue")
    };

    let mut ncinfo = NetClientOpenInfo {
        tx_buf: tx_buf.id(),
        rx_buf: rx_buf.id(),
        tx_queue: tx_queue_obj.id(),
        rx_queue: rx_queue_obj.id(),
        handle: 0,
        addr: IpAddr::from_str(IP).unwrap(),
        gateway: IpAddr::from_str(GATEWAY).unwrap(),
        hwaddr: EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
        addr_prefix_len: 8,
    };

    let ep = NetServer::open(&ncinfo)?;
    let client = Client::new(ep);

    let desc = handles
        .insert(caller, client)
        .ok_or(ResourceError::OutOfResources)?;
    ncinfo.handle = desc;
    Ok(ncinfo)
}
