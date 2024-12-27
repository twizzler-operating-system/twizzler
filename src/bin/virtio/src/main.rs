extern crate twizzler_minruntime;

mod hal;

use hal::TestHal;
use virtio_drivers::transport::Transport;

pub mod tcp;
mod transport;

use transport::TwizzlerTransport;

const NET_QUEUE_SIZE: usize = 16;

fn main() {
    virtio_net(TwizzlerTransport::new().unwrap());
}

// Taken from Virtio drivers example
fn virtio_net<T: Transport>(transport: T) {
    tcp::test_echo_server();
}
