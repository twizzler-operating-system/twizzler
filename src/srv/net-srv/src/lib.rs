#![feature(portable_simd)]
#![feature(lock_value_accessors)]

use std::{
    str::FromStr,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock, RwLock,
    },
    thread::JoinHandle,
    time::Duration,
};

use secgate::{
    util::{Descriptor, HandleMgr},
    ResourceError,
};
use smoltcp::{
    iface::{Config, Interface, SocketHandle},
    time::Instant,
    wire::{EthernetAddress, EthernetFrame, IpAddress, IpCidr, PrettyPrinter},
};
use tracing::Level;
use twizzler::object::RawObject;
use twizzler_abi::{
    object::ObjID,
    syscall::{sys_thread_sync, ObjectCreate, ThreadSync},
};
use twizzler_net::{
    packet::PacketObject, ClientMsg, ClientRet, NetClient, NetClientConfig, NetClientOpenInfo,
    NetServer, ServerMsg, ServerRet,
};
use twizzler_queue::Queue;
use twizzler_rt_abi::{error::TwzError, Result};
use virtio_net::{DeviceWrapper, TwizzlerTransport};
const IP: &str = "10.0.2.15"; // QEMU user networking default IP
const _GATEWAY: &str = "10.0.2.2"; // QEMU user networking gateway

const RX_BUF_SIZE: usize = 65536 * 2;
const TX_BUF_SIZE: usize = 8192 * 2;
static NETINFO: OnceLock<NetworkInfo> = OnceLock::new();

enum Work {
    Exit,
}

struct Client {
    ep: Mutex<NetServer>,
    jh: OnceLock<JoinHandle<()>>,
}

impl Client {
    pub fn new(ep: NetServer) -> Arc<Self> {
        let client = Arc::new(Client {
            ep: Mutex::new(ep),
            jh: OnceLock::new(),
        });
        let _client = client.clone();
        let jh = std::thread::spawn(move || client_thread(_client));
        client.jh.set(jh).unwrap();
        client
    }
}

#[allow(dead_code)]
struct NetworkInfo {
    handles: Mutex<HandleMgr<Arc<Client>>>,
    device: DeviceWrapper<TwizzlerTransport>,
}

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

    let (device, recv) = get_virtio_net_device_and_interface();
    let _device = device.clone();
    std::thread::spawn(move || device_thread(_device, recv));
    tracing::info!("network ready");

    let _ = NETINFO.set(NetworkInfo {
        handles: Mutex::new(HandleMgr::new(None)),
        device,
    });

    Ok(())
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
    handles.remove(caller, desc);
    Ok(())
}

#[secgate::entry(lib = "twizzler-net")]
pub fn twz_net_open_client(config: NetClientConfig) -> Result<NetClientOpenInfo> {
    twizzler_abi::klog_println!("open client: {:?}", config);
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
    };

    let ep = NetServer::open(&ncinfo)?;
    let client = Client::new(ep);

    let desc = handles
        .insert(caller, client)
        .ok_or(ResourceError::OutOfResources)?;
    ncinfo.handle = desc;
    Ok(ncinfo)
}

fn get_virtio_net_device_and_interface() -> (
    DeviceWrapper<TwizzlerTransport>,
    Receiver<Option<(SocketHandle, u16)>>,
) {
    let (s, r) = std::sync::mpsc::channel();
    let device = virtio_net::get_device(s);

    (device, r)
}

use smoltcp::phy::{Device, RxToken, TxToken};

fn device_thread(
    mut device: DeviceWrapper<TwizzlerTransport>,
    recv: Receiver<Option<(SocketHandle, u16)>>,
) {
    eprintln!(
        "device thread started: {:?} {:?}",
        device.mac_address(),
        device.capabilities()
    );
    loop {
        match recv.recv() {
            Err(_) => break,
            _ => {
                while let Some((rx, _tx)) = device.receive(Instant::now()) {
                    rx.consume(|buf| {
                        let f = EthernetFrame::new_unchecked(&mut *buf);
                        let pp = PrettyPrinter::<EthernetFrame<&mut [u8]>>::print(&f);
                        eprintln!("device thread got {}", pp);
                        let handles = NETINFO.get().unwrap().handles.lock().unwrap();
                        for (_, i, client) in handles.handles() {
                            let mut ep = client.ep.lock().unwrap();
                            let ctx = ep.transmit(Instant::now()).unwrap();
                            ctx.consume(buf.len(), |cbuf| cbuf.copy_from_slice(buf));
                        }
                    });
                }
            }
        }
    }
}

impl Client {
    fn active(&self) -> bool {
        // TODO
        true
    }
}

fn client_thread(client: Arc<Client>) {
    let mut device = NETINFO.get().unwrap().device.clone();
    while client.active() {
        let mut ep = client.ep.lock().unwrap();
        while let Some((rx, _tx)) = ep.receive(Instant::now()) {
            rx.consume(|buf| {
                let f = EthernetFrame::new_unchecked(&mut *buf);
                let pp = PrettyPrinter::<EthernetFrame<&mut [u8]>>::print(&f);
                eprintln!("client thread got {}", pp);
                if let Some(dtx) = device.transmit(Instant::now()) {
                    dtx.consume(buf.len(), |dbuf| dbuf.copy_from_slice(buf));
                }
            })
        }

        let rx_waiter = ep.rx_waiter();
        if ep.has_pending_msg_from_client() {
            continue;
        }
        drop(ep);

        let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(rx_waiter)], None);
    }
}
