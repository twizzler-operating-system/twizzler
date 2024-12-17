extern crate twizzler_abi;

mod hal;

use hal::TestHal;
use virtio_drivers::transport::Transport;

mod tcp;
mod transport;

use transport::TwizzlerTransport;

const NET_QUEUE_SIZE: usize = 16;

fn main() {
    virtio_net(TwizzlerTransport::new().unwrap());
}

// Taken from Virtio drivers example
fn virtio_net<T: Transport>(transport: T) {
    const NET_BUFFER_LEN: usize = 2048;
    let net = virtio_drivers::device::net::VirtIONet::<TestHal, T, NET_QUEUE_SIZE>::new(
        transport,
        NET_BUFFER_LEN,
    )
    .expect("failed to create net driver");
    println!("MAC address: {:02x?}", net.mac_address());
    tcp::test_echo_server(net);
}
