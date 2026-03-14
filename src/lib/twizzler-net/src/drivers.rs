use std::collections::HashMap;

use secgate::TwzError;
use twizzler_abi::syscall::ThreadSync;
use twizzler_driver::{
    device::Device,
    dma::{DMA_PAGE_SIZE, DmaObject, DmaOptions, DmaSliceRegion, PhysAddr},
};
use twizzler_io::packet::PacketObject;

use crate::PacketNum;

mod e1000;

pub type QueueHandle = u32;

#[derive(Default)]
pub struct Features {}

pub trait NetDriver {
    fn features(&self) -> Features {
        Features::default()
    }

    fn device(&self) -> &Device;
    fn device_mut(&mut self) -> &mut Device;

    fn setup_rx_queue(&mut self, len: usize) -> Result<QueueHandle, TwzError>;
    fn destroy_rx_queue(&mut self, queue: QueueHandle) -> Result<(), TwzError>;

    fn setup_tx_queue(&mut self, len: usize) -> Result<QueueHandle, TwzError>;
    fn destroy_tx_queue(&mut self, queue: QueueHandle) -> Result<(), TwzError>;

    fn tx_queues(&self) -> Vec<QueueHandle>;
    fn rx_queues(&self) -> Vec<QueueHandle>;

    fn rx_packet_buffer(&self, queue: QueueHandle) -> &DmaPacketObject;
    fn tx_packet_buffer(&self, queue: QueueHandle) -> &DmaPacketObject;

    fn mac_address(&self, queue: QueueHandle) -> Result<[u8; 6], TwzError>;

    fn set_mac_address(&self, _queue: QueueHandle, _addr: [u8; 6]) -> Result<(), TwzError> {
        Err(TwzError::NOT_SUPPORTED)
    }

    fn recv_packets(
        &mut self,
        queue: QueueHandle,
        packets: &mut [Packet],
    ) -> Result<usize, TwzError>;

    fn send_packets(
        &mut self,
        queue: QueueHandle,
        packets: &mut [Packet],
    ) -> Result<usize, TwzError>;

    fn has_work(&self, queue: QueueHandle) -> WorkItems;
    fn waitpoint(&self, queue: QueueHandle) -> ThreadSync;
}

#[derive(Default, Clone)]
pub struct Packet {
    po: Option<PacketObject>,
    pn: PacketNum,
    phys_addr: PhysAddr,
    len: u32,
}

bitflags::bitflags! {
    pub struct WorkItems : u32 {
        const RX_READY = 0x1;
        const TX_SENT = 0x2;
        const STATUS_CHANGE = 0x4;
        const TX_ERROR = 0x8;
        const RX_ERROR = 0x10;
    }
}

pub struct DmaPacketObject {
    po: PacketObject,
    dma: DmaObject,
    regions: HashMap<u32, DmaSliceRegion<u8>>,
    rev_addr_map: HashMap<PhysAddr, u32>,
}

impl From<PacketObject> for DmaPacketObject {
    fn from(value: PacketObject) -> Self {
        Self {
            dma: DmaObject::new(value.object().clone()),
            po: value,
            regions: HashMap::new(),
            rev_addr_map: HashMap::new(),
        }
    }
}

impl DmaPacketObject {
    pub fn packet_object(&self) -> &PacketObject {
        &self.po
    }

    pub fn dma(&self) -> &DmaObject {
        &self.dma
    }

    pub fn allocate_packet(&mut self) -> Option<(u32, PhysAddr)> {
        let num = self.packet_object().allocate_packet()?;
        Some((num, self.phys_addr(num)?))
    }

    pub fn phys_addr(&mut self, packet: u32) -> Option<PhysAddr> {
        let size = self
            .packet_object()
            .packet_size()
            .next_multiple_of(DMA_PAGE_SIZE);
        let entry = self.regions.entry(packet).or_insert_with(|| {
            let offset = self.po.packet_offset(packet);
            self.dma.slice_region::<u8>(
                offset,
                size,
                twizzler_driver::dma::Access::BiDirectional,
                DmaOptions::empty(),
            )
        });
        let pin = entry.pin().ok()?;
        if !self.rev_addr_map.contains_key(&pin.backing[0].addr()) {
            self.rev_addr_map.insert(pin.backing[0].addr(), packet);
        }
        Some(pin.backing[0].addr())
    }

    pub fn packet_num(&self, addr: PhysAddr) -> Option<u32> {
        self.rev_addr_map.get(&addr).copied()
    }

    pub fn release_packet(&self, packet: u32) {
        self.packet_object().release_packet(packet);
    }
}
