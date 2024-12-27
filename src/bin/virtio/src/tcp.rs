//! Simple echo server over TCP.
//!
//! Ref: <https://github.com/smoltcp-rs/smoltcp/blob/master/examples/server.rs>

use core::{cell::RefCell, str::FromStr};
use std::{borrow::ToOwned, rc::Rc, vec, vec::Vec};

use smoltcp::{
    iface::{Config, Interface, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::tcp,
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};
use virtio_drivers::{
    device::net::{RxBuffer, VirtIONet},
    transport::Transport,
    Error,
};

use crate::{TwizzlerTransport, TestHal, NET_QUEUE_SIZE};

type DeviceImpl<T> = VirtIONet<TestHal, T, NET_QUEUE_SIZE>;

const IP: &str = "10.0.2.15"; // QEMU user networking default IP
const GATEWAY: &str = "10.0.2.2"; // QEMU user networking gateway
const PORT: u16 = 5555;
const NET_BUFFER_LEN: usize = 2048;

pub struct DeviceWrapper<T: Transport> {
    inner: Rc<RefCell<DeviceImpl<T>>>,
}

impl<T: Transport> DeviceWrapper<T> {
    fn new(dev: DeviceImpl<T>) -> Self {
        DeviceWrapper {
            inner: Rc::new(RefCell::new(dev)),
        }
    }

    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.borrow().mac_address())
    }
}

impl<T: Transport> Device for DeviceWrapper<T> {
    type RxToken<'a>
        = VirtioRxToken<T>
    where
        Self: 'a;
    type TxToken<'a>
        = VirtioTxToken<T>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self.inner.borrow_mut().receive() {
            Ok(buf) => Some((
                VirtioRxToken(self.inner.clone(), buf),
                VirtioTxToken(self.inner.clone()),
            )),
            Err(Error::NotReady) => None,
            Err(err) => panic!("receive failed: {}", err),
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(VirtioTxToken(self.inner.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1536;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

pub struct VirtioRxToken<T: Transport>(Rc<RefCell<DeviceImpl<T>>>, RxBuffer);
pub struct VirtioTxToken<T: Transport>(Rc<RefCell<DeviceImpl<T>>>);

impl<T: Transport> RxToken for VirtioRxToken<T> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut rx_buf = self.1;
        // println!(
        //     "RECV {} bytes: {:02X?}",
        //     rx_buf.packet_len(),
        //     rx_buf.packet()
        // );
        // println!("RX BUFFER ADDR: {:p}", rx_buf.packet_mut());
        let result = f(rx_buf.packet_mut());
        self.0.borrow_mut().recycle_rx_buffer(rx_buf).unwrap();
        result
    }
}

impl<T: Transport> TxToken for VirtioTxToken<T> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut dev = self.0.borrow_mut();
        let mut tx_buf = dev.new_tx_buffer(len);
        let result = f(tx_buf.packet_mut());
        // println!("SEND {} bytes: {:02X?}", len, tx_buf.packet());
        // println!("TX BUFFER ADDR: {:p}", tx_buf.packet_mut());
        dev.send(tx_buf).unwrap();
        result
    }
}

// Gets the Virtio Net struct which implements the device used for smoltcp. Use this to create a smoltcp interface to send and receive packets.
// NOTE: Only the first device used will work properly
pub fn get_device() -> DeviceWrapper<TwizzlerTransport> {
    let net = VirtIONet::<TestHal, TwizzlerTransport, NET_QUEUE_SIZE>::new(
        TwizzlerTransport::new().unwrap(),
        NET_BUFFER_LEN,
    )
    .expect("failed to create net driver");
    DeviceWrapper::<TwizzlerTransport>::new(net)
}

pub fn test_echo_server() {
    let mut device = get_device();

    if device.capabilities().medium != Medium::Ethernet {
        panic!("This implementation only supports virtio-net which is an ethernet device");
    }

    let hardware_addr = HardwareAddress::Ethernet(device.mac_address());

    // Create interface
    let mut config = Config::new(hardware_addr);
    config.random_seed = 0x2333;

    let mut iface = Interface::new(config, &mut device, Instant::now());
    iface.update_ip_addrs(|ip_addrs| {
        ip_addrs
            .push(IpCidr::new(IpAddress::from_str(IP).unwrap(), 24))
            .unwrap();
    });

    iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::from_str(GATEWAY).unwrap())
        .unwrap();

    // Create sockets
    let tcp_rx_buffer = tcp::SocketBuffer::new(vec![0; 1024]);
    let tcp_tx_buffer = tcp::SocketBuffer::new(vec![0; 1024]);
    let tcp_socket = tcp::Socket::new(tcp_rx_buffer, tcp_tx_buffer);

    let mut sockets = SocketSet::new(vec![]);
    let tcp_handle = sockets.add(tcp_socket);

    println!("start a echo server...");
    let mut tcp_active = false;
    loop {
        let timestamp = Instant::now();

        iface.poll(timestamp, &mut device, &mut sockets);

        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if !socket.is_open() {
            println!("listening on port {}...", PORT);
            socket.listen(PORT).unwrap();
        }

        if socket.is_active() && !tcp_active {
            println!("tcp:{} connected", PORT);
        } else if !socket.is_active() && tcp_active {
            println!("tcp:{} disconnected", PORT);
        }
        tcp_active = socket.is_active();

        if socket.may_recv() {
            let data = socket
                .recv(|buffer| {
                    let recvd_len = buffer.len();
                    if !buffer.is_empty() {
                        println!("tcp:{} recv {} bytes: {:?}", PORT, recvd_len, buffer);
                        let lines = buffer
                            .split(|&b| b == b'\n')
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>();
                        let data = lines.join(&b'\n');
                        (recvd_len, data)
                    } else {
                        (0, vec![])
                    }
                })
                .unwrap();
            if socket.can_send() && !data.is_empty() {
                println!("tcp:{} send data: {:?}", PORT, data);
                socket.send_slice(&data[..]).unwrap();
            }
        } else if socket.may_send() {
            println!("tcp:{} close", PORT);
            socket.close();
        }
    }
}
