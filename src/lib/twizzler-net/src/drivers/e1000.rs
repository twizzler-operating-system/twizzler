use std::{cell::OnceCell, time::Instant};

use twizzler::error::{IoError, TwzError};
use twizzler_abi::{
    device::DeviceInterruptFlags,
    syscall::{ObjectCreate, ThreadSync},
};
use twizzler_driver::{
    device::{Device, MmioObject},
    dma::{Access, DmaOptions, DmaPool, DmaSliceRegion, PhysAddr},
};
use twizzler_io::packet::PacketObject;

use crate::drivers::{DmaPacketObject, NetDriver, Packet, WorkItems};

pub struct E1000Device {
    device: Device,
    mmio: MmioObject,
    dma: DmaPool,
    rx_desc_count: usize,
    tx_desc_count: usize,
    rx_cur: usize,
    tx_cur: usize,
    tx_buf: DmaPacketObject,
    rx_buf: DmaPacketObject,
    tx_desc: OnceCell<DmaSliceRegion<TxDesc>>,
    rx_desc: OnceCell<DmaSliceRegion<RxDesc>>,
    inum: u32,
}

impl E1000Device {
    pub fn write_register(&self, reg: u32, val: u32) {
        unsafe {
            self.mmio
                .get_mmio_offset_mut(reg as usize)
                .as_mut_ptr()
                .write(val)
        };
    }

    pub fn read_register(&self, reg: u32) -> u32 {
        unsafe { self.mmio.get_mmio_offset(reg as usize).as_ptr().read() }
    }

    fn read_mmio_byte(&self, off: usize) -> u8 {
        unsafe { self.mmio.get_mmio_offset(off).as_ptr().read() }
    }

    fn detect_eeprom(&self) -> bool {
        let mut exists = false;
        for _ in 0..1000 {
            let val = self.read_register(REG_EEPROM);
            if val & 0x10 != 0 {
                exists = true;
            } else {
                exists = false;
            }
        }
        exists
    }

    fn read_eeprom(&self, addr: u8) -> u32 {
        let val = if self.detect_eeprom() {
            self.write_register(REG_EEPROM, ((addr as u32) << 8) | 1);
            let mut val = 0;
            while val & (1 << 4) == 0 {
                val = self.read_register(REG_EEPROM);
            }
            val
        } else {
            self.write_register(REG_EEPROM, ((addr as u32) << 2) | 1);
            let mut val = 0;
            while val & (1 << 1) == 0 {
                val = self.read_register(REG_EEPROM);
            }
            val
        };
        (val >> 16) & 0xFFFF
    }

    fn read_mac_address(&self) -> Option<[u8; 6]> {
        let mut mac = [0u8; 6];
        if self.detect_eeprom() {
            let x = self.read_eeprom(0);
            mac[0] = (x & 0xff) as u8;
            mac[1] = ((x >> 8) & 0xff) as u8;
            let x = self.read_eeprom(1);
            mac[2] = (x & 0xff) as u8;
            mac[3] = ((x >> 8) & 0xff) as u8;
            let x = self.read_eeprom(2);
            mac[4] = (x & 0xff) as u8;
            mac[5] = ((x >> 8) & 0xff) as u8;
        } else {
            if self.read_register(0x5400) != 0 {
                for i in 0..6 {
                    mac[i] = self.read_mmio_byte(0x5400 + i);
                }
            } else {
                return None;
            }
        }
        Some(mac)
    }

    pub fn rxinit(&mut self) -> Result<(), TwzError> {
        let mut rx = self
            .dma
            .allocate_array(self.rx_desc_count, RxDesc::default())?;

        for i in 0..self.rx_desc_count {
            let desc = &mut unsafe { rx.get_mut() }[i];
            let region = self.rx_buf.allocate_packet().unwrap();
            desc.addr = region.1.0;
            desc.status = 0;
        }

        self.write_register(
            REG_RXDESCLO,
            (rx.pin()?.backing[0].addr().0 & 0xFFFFFFFF) as u32,
        );
        self.write_register(REG_RXDESCHI, (rx.pin()?.backing[0].addr().0 >> 32) as u32);

        self.write_register(REG_RXDESCLEN, (self.rx_desc_count * 16) as u32);
        self.write_register(REG_RXDESCHEAD, 0);
        self.write_register(REG_RXDESCTAIL, (self.rx_desc_count - 1) as u32);
        self.write_register(
            REG_RCTRL,
            RCTL_EN
                | RCTL_SBP
                | RCTL_UPE
                | RCTL_MPE
                | RCTL_LBM_NONE
                | RTCL_RDMTS_HALF
                | RCTL_BAM
                | RCTL_SECRC
                | RCTL_BSIZE_2048,
        );
        self.rx_desc.set(rx);

        Ok(())
    }

    pub fn txinit(&mut self) -> Result<(), TwzError> {
        let mut tx = self
            .dma
            .allocate_array(self.tx_desc_count, TxDesc::default())?;

        for i in 0..self.tx_desc_count {
            let desc = &mut unsafe { tx.get_mut() }[i];
            desc.status = 0;
        }

        self.write_register(
            REG_TXDESCLO,
            (tx.pin()?.backing[0].addr().0 & 0xFFFFFFFF) as u32,
        );
        self.write_register(REG_TXDESCHI, (tx.pin()?.backing[0].addr().0 >> 32) as u32);

        self.write_register(REG_TXDESCLEN, (self.tx_desc_count * 16) as u32);
        self.write_register(REG_TXDESCHEAD, 0);
        self.write_register(REG_TXDESCTAIL, 0);
        self.write_register(
            REG_TCTRL,
            TCTL_EN | TCTL_PSP | (15 << TCTL_CT_SHIFT) | (64 << TCTL_COLD_SHIFT) | TCTL_RTLC,
        );
        self.tx_desc.set(tx);

        Ok(())
    }

    fn do_send_packet(&mut self, addr: u64, len: u16, tx_desc: usize) {
        let desc = &mut unsafe { self.tx_desc.get_mut().unwrap().get_mut() }[tx_desc];
        desc.addr = addr;
        desc.len = len;
        desc.cmd = CMD_EOP | CMD_IFCS | CMD_RS;
        self.write_register(REG_TXDESCTAIL, ((tx_desc + 1) % self.tx_desc_count) as u32);
    }

    pub fn send_packets(&mut self, sends: &[SendCmd]) -> usize {
        for packet in sends {
            self.do_send_packet(packet.addr, packet.len, self.tx_cur);
            self.tx_cur = (self.tx_cur + 1) % self.tx_desc_count;
        }
        sends.len()
    }

    fn do_recv_packets(&mut self, mut rx_desc: usize, mut descs: &mut [RxDesc]) -> usize {
        let mut count = 0;
        while descs.len() != 0 {
            let cur = &unsafe { self.rx_desc.get().unwrap().get() }[rx_desc];
            if cur.status & 0x1 == 0 {
                return count;
            }
            descs[0] = *cur;
            count += 1;
            rx_desc = (rx_desc + 1) % self.rx_desc_count;
            descs = &mut descs[1..];
        }
        count
    }

    pub fn recv_packets(&mut self, descs: &mut [RxDesc]) -> usize {
        let count = self.do_recv_packets(self.rx_cur, descs);
        if count == 0 {
            return 0;
        }

        let doorbell = (self.rx_cur + (count - 1)) % self.rx_desc_count;
        self.write_register(REG_RXDESCTAIL, doorbell as u32);

        self.rx_cur += count;
        count
    }

    pub fn init(
        device: Device,
        rx_desc_count: usize,
        tx_desc_count: usize,
    ) -> Result<Self, TwzError> {
        let (msiint, devint) = device.allocate_interrupt(0)?;

        device.repr_mut().register_interrupt(
            devint as usize,
            msiint,
            DeviceInterruptFlags::empty(),
        );
        let mmio = device.find_mmio_bar(0).ok_or(TwzError::NOT_SUPPORTED)?;

        let mut this = Self {
            device,
            mmio,
            dma: DmaPool::new(
                DmaPool::default_spec(),
                Access::BiDirectional,
                DmaOptions::empty(),
            ),
            rx_desc_count,
            tx_desc_count,
            rx_cur: 0,
            tx_cur: 0,
            tx_desc: OnceCell::new(),
            rx_desc: OnceCell::new(),
            inum: devint,
            tx_buf: PacketObject::new(ObjectCreate::default(), QUEUE_LEN, 4096)?.into(),
            rx_buf: PacketObject::new(ObjectCreate::default(), QUEUE_LEN, 4096)?.into(),
        };

        this.write_register(REG_IMASKCLEAR, 0xFFFFFFFF);
        let ctl = this.read_register(REG_CTRL);
        let new_ctl = ctl & !(CTL_RFCE | CTL_TFCE | CTL_SLU);
        this.write_register(REG_CTRL, new_ctl | CTL_RST);

        let start = Instant::now();
        while this.read_register(REG_CTRL) & CTL_RST != 0 {
            if start.elapsed().as_secs() > 1 {
                return Err(IoError::DeviceError.into());
            }
        }
        this.write_register(REG_IMASKCLEAR, 0xFFFFFFFF);

        this.rxinit()?;
        this.txinit()?;

        this.write_register(REG_RDTR, 0);
        this.write_register(REG_RADV, 0);

        this.write_register(REG_ICR, 0xFFFFFFFF);
        this.read_register(REG_ICR);
        this.write_register(REG_IMASKSET, (1 << 7) | (1 << 2));

        this.write_register(REG_CTRL, new_ctl | CTL_FD | CTL_RFCE | CTL_TFCE | CTL_SLU);

        Ok(this)
    }

    pub fn check_interrupts(&self) -> u32 {
        self.read_register(REG_ICR)
    }
}

const QUEUE_LEN: usize = 32;

impl NetDriver for E1000Device {
    fn device(&self) -> &Device {
        &self.device
    }

    fn device_mut(&mut self) -> &mut Device {
        &mut self.device
    }

    fn setup_rx_queue(&mut self, _len: usize) -> Result<super::QueueHandle, TwzError> {
        Ok(0)
    }

    fn destroy_rx_queue(&mut self, _queue: super::QueueHandle) -> Result<(), TwzError> {
        Ok(())
    }

    fn setup_tx_queue(&mut self, _len: usize) -> Result<super::QueueHandle, TwzError> {
        Ok(0)
    }

    fn destroy_tx_queue(&mut self, _queue: super::QueueHandle) -> Result<(), TwzError> {
        Ok(())
    }

    fn recv_packets(
        &mut self,
        _queue: super::QueueHandle,
        packets: &mut [super::Packet],
    ) -> Result<usize, TwzError> {
        let mut descs = [RxDesc::default(); QUEUE_LEN];
        let count = self.recv_packets(&mut descs[0..packets.len()]);
        for i in 0..count {
            packets[i].po = Some(self.rx_buf.packet_object().clone());
            packets[i].pn = self.rx_buf.packet_num(PhysAddr(descs[i].addr)).unwrap();
            packets[i].phys_addr = PhysAddr(descs[i].addr);
            packets[i].len = descs[i].len as u32;
        }

        Ok(count)
    }

    fn send_packets(
        &mut self,
        _queue: super::QueueHandle,
        packets: &mut [super::Packet],
    ) -> Result<usize, TwzError> {
        let mut descs = [SendCmd::default(); QUEUE_LEN];
        for i in 0..packets.len().min(QUEUE_LEN) {
            descs[i] = (&packets[i]).into();
        }
        let count = self.send_packets(&mut descs[0..packets.len().min(QUEUE_LEN)]);
        Ok(count)
    }

    fn has_work(&self, _queue: super::QueueHandle) -> WorkItems {
        let int = self.check_interrupts();
        let mut work = WorkItems::empty();
        if int & ICR_RX_READY != 0 {
            work.insert(WorkItems::RX_READY);
        }
        if int & ICR_TX_DONE != 0 {
            work.insert(WorkItems::TX_SENT);
        }
        if int & ICR_RX_ERROR != 0 {
            work.insert(WorkItems::RX_ERROR);
        }
        if int & ICR_TX_ERROR != 0 {
            work.insert(WorkItems::TX_ERROR);
        }
        if int & ICR_LSU != 0 {
            work.insert(WorkItems::STATUS_CHANGE);
        }
        work
    }

    fn waitpoint(&self, _queue: super::QueueHandle) -> ThreadSync {
        let wp = self.device.repr().setup_interrupt_sleep(self.inum as usize);
        ThreadSync::new_sleep(wp)
    }

    fn tx_queues(&self) -> Vec<super::QueueHandle> {
        vec![0]
    }

    fn rx_queues(&self) -> Vec<super::QueueHandle> {
        vec![0]
    }

    fn mac_address(&self, _queue: super::QueueHandle) -> Result<[u8; 6], TwzError> {
        self.read_mac_address().ok_or(TwzError::NOT_SUPPORTED)
    }

    fn rx_packet_buffer(&self, _queue: super::QueueHandle) -> &DmaPacketObject {
        &self.rx_buf
    }

    fn tx_packet_buffer(&self, _queue: super::QueueHandle) -> &DmaPacketObject {
        &self.tx_buf
    }
}

#[repr(C, packed)]
#[derive(Default, Copy, Clone, Debug)]
pub struct RxDesc {
    addr: u64,
    len: u16,
    csum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C, packed)]
#[derive(Default, Copy, Clone, Debug)]
pub struct TxDesc {
    addr: u64,
    len: u16,
    cso: u16,
    cmd: u16,
    status: u8,
    css: u8,
    special: u16,
}

#[derive(Default, Clone, Copy)]
pub struct SendCmd {
    pub addr: u64,
    pub len: u16,
}

impl From<&Packet> for SendCmd {
    fn from(value: &Packet) -> Self {
        Self {
            addr: value.phys_addr.0,
            len: value.len as u16,
        }
    }
}

// Register addresses
const REG_CTRL: u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EEPROM: u32 = 0x0014;
const REG_CTRL_EXT: u32 = 0x0018;
const REG_ICR: u32 = 0x00C0;
const REG_ICS: u32 = 0x00C8;
const REG_IMASKSET: u32 = 0x00D0;
const REG_IMASKCLEAR: u32 = 0x00D8;
const REG_RCTRL: u32 = 0x0100;
const REG_RXDESCLO: u32 = 0x2800;
const REG_RXDESCHI: u32 = 0x2804;
const REG_RXDESCLEN: u32 = 0x2808;
const REG_RXDESCHEAD: u32 = 0x2810;
const REG_RXDESCTAIL: u32 = 0x2818;

const REG_TCTRL: u32 = 0x0400;
const REG_TXDESCLO: u32 = 0x3800;
const REG_TXDESCHI: u32 = 0x3804;
const REG_TXDESCLEN: u32 = 0x3808;
const REG_TXDESCHEAD: u32 = 0x3810;
const REG_TXDESCTAIL: u32 = 0x3818;

const REG_RDTR: u32 = 0x2820; // RX Delay Timer Register
const REG_RXDCTL: u32 = 0x2828; // RX Descriptor Control
const REG_RADV: u32 = 0x282C; // RX Int. Absolute Delay Timer
const REG_RSRPD: u32 = 0x2C00; // RX Small Packet Detect Interrupt

const REG_TIPG: u32 = 0x0410; // Transmit Inter Packet Gap

const ICR_RX_READY: u32 = 0x80;
const ICR_TX_DONE: u32 = 0x1;
const ICR_RX_ERROR: u32 = 0x8;
const ICR_TX_ERROR: u32 = 0;
const ICR_LSU: u32 = 0x4;

// Control Register
const CTL_RST: u32 = 1 << 26; // Reset
const CTL_RFCE: u32 = 1 << 27; // Receive Flow Control Enable
const CTL_TFCE: u32 = 1 << 28; // Transmit Flow Control Enable
const CTL_FD: u32 = 1 << 0; // Full Duplex
const CTL_SLU: u32 = 1 << 6; // Set Link Up

// Receive Control Register
const RCTL_EN: u32 = 1 << 1; // Receiver Enable
const RCTL_SBP: u32 = 1 << 2; // Store Bad Packets
const RCTL_UPE: u32 = 1 << 3; // Unicast Promiscuous Enabled
const RCTL_MPE: u32 = 1 << 4; // Multicast Promiscuous Enabled
const RCTL_LPE: u32 = 1 << 5; // Long Packet Reception Enable
const RCTL_LBM_NONE: u32 = 0 << 6; // No Loopback
const RCTL_LBM_PHY: u32 = 3 << 6; // PHY or external SerDesc loopback
const RTCL_RDMTS_HALF: u32 = 0 << 8; // Free Buffer Threshold is 1/2 of RDLEN
const RTCL_RDMTS_QUARTER: u32 = 1 << 8; // Free Buffer Threshold is 1/4 of RDLEN
const RTCL_RDMTS_EIGHTH: u32 = 2 << 8; // Free Buffer Threshold is 1/8 of RDLEN
const RCTL_MO_36: u32 = 0 << 12; // Multicast Offset - bits 47:36
const RCTL_MO_35: u32 = 1 << 12; // Multicast Offset - bits 46:35
const RCTL_MO_34: u32 = 2 << 12; // Multicast Offset - bits 45:34
const RCTL_MO_32: u32 = 3 << 12; // Multicast Offset - bits 43:32
const RCTL_BAM: u32 = 1 << 15; // Broadcast Accept Mode
const RCTL_VFE: u32 = 1 << 18; // VLAN Filter Enable
const RCTL_CFIEN: u32 = 1 << 19; // Canonical Form Indicator Enable
const RCTL_CFI: u32 = 1 << 20; // Canonical Form Indicator Bit Value
const RCTL_DPF: u32 = 1 << 22; // Discard Pause Frames
const RCTL_PMCF: u32 = 1 << 23; // Pass MAC Control Frames
const RCTL_SECRC: u32 = 1 << 26; // Strip Ethernet CRC

// Buffer Sizes
const RCTL_BSIZE_256: u32 = 3 << 16;
const RCTL_BSIZE_512: u32 = 2 << 16;
const RCTL_BSIZE_1024: u32 = 1 << 16;
const RCTL_BSIZE_2048: u32 = 0 << 16;
const RCTL_BSIZE_4096: u32 = (3 << 16) | (1 << 25);
const RCTL_BSIZE_8192: u32 = (2 << 16) | (1 << 25);
const RCTL_BSIZE_16384: u32 = (1 << 16) | (1 << 25);

// Transmit Command
const CMD_EOP: u16 = 1 << 0; // End of Packet
const CMD_IFCS: u16 = 1 << 1; // Insert FCS
const CMD_IC: u16 = 1 << 2; // Insert Checksum
const CMD_RS: u16 = 1 << 3; // Report Status
const CMD_RPS: u16 = 1 << 4; // Report Packet Sent
const CMD_VLE: u16 = 1 << 6; // VLAN Packet Enable
const CMD_IDE: u16 = 1 << 7; // Interrupt Delay Enable

// TCTL Register
const TCTL_EN: u32 = 1 << 1; // Transmit Enable
const TCTL_PSP: u32 = 1 << 3; // Pad Short Packets
const TCTL_CT_SHIFT: u32 = 4; // Collision Threshold
const TCTL_COLD_SHIFT: u32 = 12; // Collision Distance
const TCTL_SWXOFF: u32 = 1 << 22; // Software XOFF Transmission
const TCTL_RTLC: u32 = 1 << 24; // Re-transmit on Late Collision

const TSTA_DD: u32 = 1 << 0; // Descriptor Done
const TSTA_EC: u32 = 1 << 1; // Excess Collisions
const TSTA_LC: u32 = 1 << 2; // Late Collision
const LSTA_TU: u32 = 1 << 3; // Transmit Underrun
