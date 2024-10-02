extern crate twizzler_abi;

use twizzler_abi::device::BusType;
use twizzler_driver::bus::pcie::{PcieDeviceInfo, PcieFunctionHeader};

mod hal;

use hal::TestHal;
use virtio_drivers::transport::{
    pci::{
        bus::{BarInfo, Cam, Command, DeviceFunction, PciRoot},
        virtio_device_type, PciTransport,
    },
    DeviceType, Transport,
};

mod transport;

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
    let mut net =
        virtio_drivers::device::net::VirtIONetRaw::<TestHal, T, NET_QUEUE_SIZE>::new(transport)
            .expect("failed to create net driver");
    println!("MAC address: {:02x?}", net.mac_address());

    let mut buf = [0u8; 2048];
    let (hdr_len, pkt_len) = net.receive_wait(&mut buf).expect("failed to recv");
    println!(
        "recv {} bytes: {:02x?}",
        pkt_len,
        &buf[hdr_len..hdr_len + pkt_len]
    );
    net.send(&buf[..hdr_len + pkt_len]).expect("failed to send");
    println!("virtio-net test finished");
}
