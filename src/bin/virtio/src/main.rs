extern crate twizzler_abi;

use twizzler_abi::device::BusType;
use twizzler_driver::bus::pcie::PcieDeviceInfo;

mod hal;

use hal::TestHal;
use virtio_drivers::transport::Transport;

mod transport;
mod tcp;

use transport::TwizzlerTransport;

const NET_QUEUE_SIZE: usize = 16;

fn main() {
    let transport = init_virtio_net();
    virtio_net(transport);
}

// Taken from devmgr crate
fn get_pcie_offset(bus: u8, device: u8, function: u8) -> usize {
    ((bus as usize * 256) + (device as usize * 8) + function as usize) * 4096
}

// Finds the virtio-net device and creates a transport to facilitate the driver.
fn init_virtio_net() -> TwizzlerTransport {
    println!("Searching for virtio-net device");

    let device_root = twizzler_driver::get_bustree_root();
    for device in device_root.children() {
        if device.is_bus() && device.bus_type() == BusType::Pcie {
            for child in device.children() {
                let info = unsafe { child.get_info::<PcieDeviceInfo>(0).unwrap() };
                if info.get_data().class == 2
                    && info.get_data().subclass == 0
                    && info.get_data().progif == 0
                    && info.get_data().vendor_id == 0x1AF4
                {
                    println!("Found VirtIO networking device!");

                    return TwizzlerTransport::new(child).unwrap();
                }
            }
        }
    }
    panic!("No networking device found");
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
