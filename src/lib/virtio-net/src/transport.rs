use core::{
    mem::{align_of, size_of},
    ptr::NonNull,
};
use std::sync::Arc;

use twizzler_abi::{
    device::{bus::pcie::PcieDeviceInfo, DeviceInterruptFlags, InterruptVector},
    syscall::{sys_thread_sync, ThreadSync},
};
use twizzler_driver::{bus::pcie::PcieCapability, device::Device, DeviceController};
use virtio_drivers::{
    transport::{pci::VirtioPciError, DeviceStatus, DeviceType, Transport},
    Error,
};
use virtio_pcie::{VirtioIsrStatus, VirtioPciNotifyCap};
use volatile::{map_field, VolatilePtr};

pub mod virtio_pcie;
use self::virtio_pcie::{CfgLocation, VirtioCfgType, VirtioCommonCfg, VirtioPciCap};

pub struct TwizzlerTransport {
    device: Arc<Device>,

    common_cfg: CfgLocation,

    notify_region: CfgLocation,
    notify_offset_multiplier: u32,

    isr_status: CfgLocation,

    config_space: Option<NonNull<[u32]>>,
}

unsafe impl Send for TwizzlerTransport {}

fn get_device() -> Device {
    let device_root = twizzler_driver::get_bustree_root();
    for device in device_root.children() {
        if device.is_bus() && device.bus_type() == twizzler_abi::device::BusType::Pcie {
            for child in device.children() {
                let info = unsafe { child.get_info::<PcieDeviceInfo>(0).unwrap() };
                // Can be modified later to let us select any other virtio device we want. For now,
                // just network is good.
                if info.get_data().class == 2
                    && info.get_data().subclass == 0
                    && info.get_data().progif == 0
                    && info.get_data().vendor_id == 0x1AF4
                {
                    println!("Found VirtIO networking device!");

                    return child;
                }
            }
        }
    }
    panic!("No VirtIO networking device found");
}

impl TwizzlerTransport {
    pub fn new(notifier: std::sync::mpsc::Sender<()>) -> Result<Self, VirtioPciError> {
        let device = Arc::new(get_device());
        let int = device.allocate_interrupt(0).unwrap();
        device
            .repr_mut()
            .register_interrupt(int.1 as usize, int.0, DeviceInterruptFlags::empty());
        let int_device = device.clone();

        let info = unsafe { device.get_info::<PcieDeviceInfo>(0).unwrap() };
        if info.get_data().vendor_id != 0x1AF4 {
            println!("Vendor ID: {}", info.get_data().vendor_id);
            return Err(VirtioPciError::InvalidVendorId(info.get_data().vendor_id));
        }

        let mut common_cfg = None;
        let mut notify_region = None;
        let mut notify_offset_multiplier = 0;
        let mut isr_status = None;
        let mut config_space = None;

        let mm = device.find_mmio_bar(0xff).unwrap();
        for cap in device.pcie_capabilities(&mm).unwrap() {
            let off: usize = match cap {
                PcieCapability::VendorSpecific(x) => x,
                _ => {
                    continue;
                }
            };

            let mut virtio_cfg_ref = unsafe { mm.get_mmio_offset_mut::<VirtioPciCap>(off) };
            let virtio_cfg = virtio_cfg_ref.as_mut_ptr();
            match map_field!(virtio_cfg.cfg_type).read() {
                VirtioCfgType::CommonCfg if common_cfg.is_none() => {
                    println!(
                        "Common CFG found! Bar: {:?}, Offset: {:?}, Length: {:?}",
                        map_field!(virtio_cfg.bar).read(),
                        map_field!(virtio_cfg.offset).read(),
                        map_field!(virtio_cfg.length).read()
                    );
                    common_cfg = Some(CfgLocation {
                        bar: map_field!(virtio_cfg.bar).read() as usize,
                        offset: map_field!(virtio_cfg.offset).read() as usize,
                        length: map_field!(virtio_cfg.length).read() as usize,
                    });
                }
                VirtioCfgType::NotifyCfg if notify_region.is_none() => {
                    let mut notify_ref =
                        unsafe { mm.get_mmio_offset_mut::<VirtioPciNotifyCap>(off) };
                    let notify_cap = notify_ref.as_mut_ptr();
                    notify_offset_multiplier = map_field!(notify_cap.notify_off_multiplier).read();
                    println!("Notify CFG found! Bar: {:?}, Offset: {:?}, Length: {:?}, Offset multiplier: {:?}", map_field!(virtio_cfg.bar).read(), map_field!(virtio_cfg.offset).read(), map_field!(virtio_cfg.length).read(), notify_offset_multiplier);
                    notify_region = Some(CfgLocation {
                        bar: map_field!(virtio_cfg.bar).read() as usize,
                        offset: map_field!(virtio_cfg.offset).read() as usize,
                        length: map_field!(virtio_cfg.length).read() as usize,
                    })
                }

                VirtioCfgType::IsrCfg if isr_status.is_none() => {
                    println!(
                        "ISR CFG found! Bar: {:?}, Offset: {:?}, Length: {:?}",
                        map_field!(virtio_cfg.bar).read(),
                        map_field!(virtio_cfg.offset).read(),
                        map_field!(virtio_cfg.length).read()
                    );
                    isr_status = Some(CfgLocation {
                        bar: map_field!(virtio_cfg.bar).read() as usize,
                        offset: map_field!(virtio_cfg.offset).read() as usize,
                        length: map_field!(virtio_cfg.length).read() as usize,
                    });
                }

                VirtioCfgType::DeviceCfg if config_space.is_none() => {
                    println!(
                        "Device CFG found! Bar: {:?}, Offset: {:?}, Length: {:?}",
                        map_field!(virtio_cfg.bar).read(),
                        map_field!(virtio_cfg.offset).read(),
                        map_field!(virtio_cfg.length).read()
                    );
                    let bar_num = map_field!(virtio_cfg.bar).read() as usize;
                    let bar = device.find_mmio_bar(bar_num).unwrap();
                    let mut start = unsafe {
                        bar.get_mmio_offset_mut::<u32>(map_field!(virtio_cfg.offset).read() as usize)
                    };
                    let len = map_field!(virtio_cfg.length).read() as usize;

                    let ptr = unsafe {
                        NonNull::from(core::slice::from_raw_parts_mut(
                            start.as_mut_ptr().as_raw_ptr().as_ptr(),
                            len,
                        ))
                    };
                    config_space = Some(ptr);
                }
                _ => {}
            }
        }
        let common_cfg = common_cfg.ok_or(VirtioPciError::MissingCommonConfig)?;
        let notify_region = notify_region.ok_or(VirtioPciError::MissingNotifyConfig)?;
        let isr_status = isr_status.ok_or(VirtioPciError::MissingIsrConfig)?;

        let thread = std::thread::spawn(move || loop {
            if int_device.repr().check_for_interrupt(0).is_some() {
                //println!("virtio int: ready");
                notifier.send(());
            }

            /*
            let bar = int_device.find_mmio_bar(isr_status.bar).unwrap();
            let mut reference =
                unsafe { bar.get_mmio_offset_mut::<VirtioIsrStatus>(isr_status.offset) };
            let ptr = reference.as_mut_ptr();

            let status = ptr.read();
            if status & 0x3 != 0 {

            }
            */

            let int_sleep = int_device.repr().setup_interrupt_sleep(0);
            let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(int_sleep)], None);
        });

        Ok(Self {
            device,
            common_cfg,
            notify_region,
            notify_offset_multiplier,
            isr_status,
            config_space,
        })
    }
}

impl Transport for TwizzlerTransport {
    fn device_type(&self) -> DeviceType {
        device_type(
            unsafe { self.device.get_info::<PcieDeviceInfo>(0) }
                .unwrap()
                .get_data()
                .device_id,
        )
    }

    fn read_device_features(&mut self) -> u64 {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.device_feature_select).write(0);
        let mut device_feature_bits = map_field!(ptr.device_feature).read() as u64;
        map_field!(ptr.device_feature_select).write(1);
        device_feature_bits |= (map_field!(ptr.device_feature).read() as u64) << 32;
        device_feature_bits
    }

    fn write_driver_features(&mut self, driver_features: u64) {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.driver_feature_select).write(0);
        map_field!(ptr.driver_feature).write(driver_features as u32);
        map_field!(ptr.driver_feature_select).write(1);
        map_field!(ptr.driver_feature).write((driver_features >> 32) as u32);
    }

    fn max_queue_size(&mut self, queue: u16) -> u32 {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.queue_select).write(queue);
        map_field!(ptr.queue_size).read().into()
    }

    fn notify(&mut self, queue: u16) {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.queue_select).write(queue);

        let queue_notify_off = map_field!(ptr.queue_notify_off).read();

        let offset_bytes = queue_notify_off as usize * self.notify_offset_multiplier as usize;
        let index = offset_bytes / size_of::<u16>();

        let notify_bar = self.device.find_mmio_bar(self.notify_region.bar).unwrap();
        let start = unsafe {
            notify_bar
                .get_mmio_offset_mut::<u16>(self.notify_region.offset as usize)
                .as_mut_ptr()
                .as_raw_ptr()
                .as_ptr()
        };

        let notify_ptr = unsafe {
            VolatilePtr::new(NonNull::from(core::slice::from_raw_parts_mut(
                start,
                self.notify_region.length as usize,
            )))
        };

        let to_write = notify_ptr.index(index);
        to_write.write(queue);
    }

    fn get_status(&self) -> virtio_drivers::transport::DeviceStatus {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        let status = map_field!(ptr.device_status).read();
        DeviceStatus::from_bits_truncate(status.into())
    }

    fn set_status(&mut self, status: virtio_drivers::transport::DeviceStatus) {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.device_status).write(status.bits() as u8);
    }

    fn set_guest_page_size(&mut self, _guest_page_size: u32) {
        // No-op, the PCI transport doesn't care.
    }

    fn requires_legacy_layout(&self) -> bool {
        false
    }

    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: virtio_drivers::PhysAddr,
        driver_area: virtio_drivers::PhysAddr,
        device_area: virtio_drivers::PhysAddr,
    ) {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.config_msix_vector).write(0);
        map_field!(ptr.queue_select).write(queue);
        map_field!(ptr.queue_size).write(size as u16);
        map_field!(ptr.queue_desc).write(descriptors.try_into().unwrap());
        map_field!(ptr.queue_driver).write(driver_area.try_into().unwrap());
        map_field!(ptr.queue_device).write(device_area.try_into().unwrap());
        map_field!(ptr.queue_msix_vector).write(0);
        map_field!(ptr.queue_enable).write(1);
    }

    fn queue_unset(&mut self, _queue: u16) {
        // The VirtIO spec doesn't allow queues to be unset once they have been set up for the PCI
        // transport, so this is a no-op.
    }

    fn queue_used(&mut self, queue: u16) -> bool {
        let bar = self.device.find_mmio_bar(self.common_cfg.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioCommonCfg>(self.common_cfg.offset) };
        let ptr = reference.as_mut_ptr();

        map_field!(ptr.queue_select).write(queue);
        map_field!(ptr.queue_enable).read() == 1
    }

    fn ack_interrupt(&mut self) -> bool {
        let bar = self.device.find_mmio_bar(self.isr_status.bar).unwrap();
        let mut reference =
            unsafe { bar.get_mmio_offset_mut::<VirtioIsrStatus>(self.isr_status.offset) };
        let ptr = reference.as_mut_ptr();

        let status = ptr.read();
        status & 0x3 != 0
    }

    //Taken from the provided virtio drivers pci transport
    fn config_space<T: 'static>(&self) -> virtio_drivers::Result<NonNull<T>> {
        if let Some(config_space) = self.config_space {
            if size_of::<T>() > config_space.len() * size_of::<u32>() {
                Err(Error::ConfigSpaceTooSmall)
            } else if align_of::<T>() > 4 {
                // Panic as this should only happen if the driver is written incorrectly.
                panic!(
                    "Driver expected config space alignment of {} bytes, but VirtIO only guarantees 4 byte alignment.",
                    align_of::<T>()
                );
            } else {
                // TODO: Use NonNull::as_non_null_ptr once it is stable.
                let config_space_ptr = NonNull::new(config_space.as_ptr() as *mut u32).unwrap();
                Ok(config_space_ptr.cast())
            }
        } else {
            Err(Error::ConfigSpaceMissing)
        }
    }
}

impl Drop for TwizzlerTransport {
    fn drop(&mut self) {
        // Disable the device
        self.set_status(DeviceStatus::empty());
        while self.get_status() != DeviceStatus::empty() {
            // Wait for the device to acknowledge the status change
            core::hint::spin_loop();
        }
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
