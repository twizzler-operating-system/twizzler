use std::{
    net::IpAddr,
    str::FromStr,
    sync::{atomic::Ordering, Mutex},
};

use secgate::{util::HandleMgr, ResourceError, TwzError};
use tracing::Level;
use twizzler::{object::RawObject, Result};
use twizzler_abi::syscall::ObjectCreate;
use twizzler_net::{
    packet::PacketObject, ClientMsg, ClientRet, NetClientConfig, NetClientOpenInfo, NetServer,
    ServerMsg, ServerRet,
};

use crate::{
    addr::AddrAssigner, client::Client, device::device_thread, port::PortAssigner, NetworkInfo,
    ADDRS, NETINFO, PORTS,
};

const GATEWAY: &str = "10.0.2.2"; // QEMU user networking gateway

#[secgate::entry(lib = "twizzler-net")]
pub fn start_network() -> Result<()> {
    if NETINFO.get().is_some() {
        eprintln!("cannot call start_network more than once");
        return Err(TwzError::NOT_SUPPORTED);
    }
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    let device = virtio_net::get_device();
    let _device = device.clone();
    std::thread::spawn(move || device_thread(_device));
    tracing::info!("network ready: gateway = {}", GATEWAY);

    let _ = PORTS.set(PortAssigner::new());
    let _ = ADDRS.set(AddrAssigner::new());

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

    let mut ports = client.ports.lock().unwrap();
    let port = if let Some(port) = port {
        if !ports.contains_key(&port) {
            if PORTS.get().unwrap().allocate_port(port) {
                Some(port)
            } else {
                None
            }
        } else {
            Some(port)
        }
    } else {
        PORTS.get().unwrap().get_ephemeral_port()
    };
    let Some(port) = port else {
        return Err(ResourceError::OutOfResources.into());
    };

    *ports.entry(port).or_default() += 1;
    Ok(port)
}

#[secgate::entry(lib = "twizzler-net")]
fn twz_net_release_port(desc: secgate::util::Descriptor, port: u16) -> Result<()> {
    twizzler_abi::klog_println!("twz_net_release_port: desc = {}, port = {}", desc, port);
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
    let mut ports = client.ports.lock().unwrap();
    // Releasing a port this client never held is a caller error, not something to underflow on:
    // `entry(port).or_default()` followed by `-= 1` panics on a debug build and wraps to
    // usize::MAX on a release one, and either way it corrupts the refcount for that port.
    let Some(entry) = ports.get_mut(&port) else {
        return Err(TwzError::INVALID_ARGUMENT);
    };
    *entry -= 1;
    if *entry == 0 {
        PORTS.get().unwrap().return_port(port);
        ports.remove(&port);
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
            PORTS.get().unwrap().return_port(port.0);
        }
        ADDRS.get().unwrap().release(client.addr);
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

    // Slot size, not frame size: a slot must hold the largest frame either side can hand over, and
    // `NetServerTxToken::consume` *panics* if it cannot (server.rs), so this bounds the MTU any
    // arm may advertise. Held at 16384 across every arm of the MSS sweep (prereg-mss-0827.md) so
    // pool geometry is common-mode and only the advertised MTU varies. Slots are demand-paged
    // object memory, so the unused tail costs address space, not frames. (Checked against the
    // round-wedge rate in `ctrl0`: 2048 vs 16384 is Fisher p = 1.000, so this is not the wedge.)
    const SLOT: usize = 16384;
    let tx_buf = PacketObject::new(ObjectCreate::default(), 1024, SLOT)?;
    let rx_buf = PacketObject::new(ObjectCreate::default(), 1024, SLOT)?;

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

    // Each client gets its own address and MAC; see addr.rs for why sharing them was actively
    // harmful rather than merely untidy.
    let addr = ADDRS
        .get()
        .ok_or(TwzError::NOT_SUPPORTED)?
        .allocate()
        .ok_or(ResourceError::OutOfResources)?;

    let mut ncinfo = NetClientOpenInfo {
        tx_buf: tx_buf.id(),
        rx_buf: rx_buf.id(),
        tx_queue: tx_queue_obj.id(),
        rx_queue: rx_queue_obj.id(),
        handle: 0,
        addr: IpAddr::from(addr.ipv4()),
        gateway: IpAddr::from_str(GATEWAY).unwrap(),
        hwaddr: addr.hwaddr(),
        addr_prefix_len: 8,
    };
    tracing::info!(
        "new net client: addr = {}, hwaddr = {}",
        ncinfo.addr,
        ncinfo.hwaddr
    );

    let ep = NetServer::open(&ncinfo)?;
    let client = Client::new(ep, addr);

    let desc = handles
        .insert(caller, client)
        .ok_or(ResourceError::OutOfResources)?;
    ncinfo.handle = desc;
    Ok(ncinfo)
}
