extern crate twizzler_abi;

use twizzler_abi::device;
use virtio_drivers::transport::{Transport, DeviceType};
use virtio_drivers::transport::pci::VirtioPciError;

use twizzler_abi::device::bus::pcie::{PcieDeviceInfo, PcieFunctionHeader};
use twizzler_driver::device::Device;


struct TwizzlerTransport {
    device: Device,
    info: PcieDeviceInfo,
}

impl TwizzlerTransport {
    pub fn new(device: Device) -> Result<Self, VirtioPciError> {
        let info = unsafe { device.get_info::<PcieDeviceInfo>(0).unwrap().get_data() };
        if (info.vendor_id != 0x1AF4) {
            return Err("Not a VirtIO device");
        }
        Ok(Self {
            device,
            info,
        })
    }
}

impl Transport for TwizzlerTransport {
    fn device_type(&self) -> DeviceType {
        device_type(info.get_data().class())
    }
    
    fn read_device_features(&mut self) -> u64 {
        todo!()
    }
    
    fn write_driver_features(&mut self, driver_features: u64) {
        todo!()
    }
    
    fn max_queue_size(&mut self, queue: u16) -> u32 {
        todo!()
    }
    
    fn notify(&mut self, queue: u16) {
        todo!()
    }
    
    fn get_status(&self) -> virtio_drivers::transport::DeviceStatus {
        todo!()
    }
    
    fn set_status(&mut self, status: virtio_drivers::transport::DeviceStatus) {
        todo!()
    }
    
    fn set_guest_page_size(&mut self, guest_page_size: u32) {
        todo!()
    }
    
    fn requires_legacy_layout(&self) -> bool {
        todo!()
    }
    
    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: virtio_drivers::PhysAddr,
        driver_area: virtio_drivers::PhysAddr,
        device_area: virtio_drivers::PhysAddr,
    ) {
        todo!()
    }
    
    fn queue_unset(&mut self, queue: u16) {
        todo!()
    }
    
    fn queue_used(&mut self, queue: u16) -> bool {
        todo!()
    }
    
    fn ack_interrupt(&mut self) -> bool {
        todo!()
    }
    
    fn config_space<T: 'static>(&self) -> virtio_drivers::Result<std::ptr::NonNull<T>> {
        todo!()
    }

    
}

/// The offset to add to a VirtIO device ID to get the corresponding PCI device ID.
const PCI_DEVICE_ID_OFFSET: u16 = 0x1040;

const TRANSITIONAL_NETWORK: u16 = 0x1000;
const TRANSITIONAL_BLOCK: u16 = 0x1001;
const TRANSITIONAL_MEMORY_BALLOONING: u16 = 0x1002;
const TRANSITIONAL_CONSOLE: u16 = 0x1003;
const TRANSITIONAL_SCSI_HOST: u16 = 0x1004;
const TRANSITIONAL_ENTROPY_SOURCE: u16 = 0x1005;
const TRANSITIONAL_9P_TRANSPORT: u16 = 0x1009;

fn device_type(pci_device_id: u16) -> DeviceType {
    match pci_device_id {
        TRANSITIONAL_NETWORK => DeviceType::Network,
        TRANSITIONAL_BLOCK => DeviceType::Block,
        TRANSITIONAL_MEMORY_BALLOONING => DeviceType::MemoryBalloon,
        TRANSITIONAL_CONSOLE => DeviceType::Console,
        TRANSITIONAL_SCSI_HOST => DeviceType::ScsiHost,
        TRANSITIONAL_ENTROPY_SOURCE => DeviceType::EntropySource,
        TRANSITIONAL_9P_TRANSPORT => DeviceType::_9P,
        id if id >= PCI_DEVICE_ID_OFFSET => DeviceType::from(id - PCI_DEVICE_ID_OFFSET),
        _ => DeviceType::Invalid,
    }
}