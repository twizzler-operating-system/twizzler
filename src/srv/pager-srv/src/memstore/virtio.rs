use std::sync::Arc;

use async_io::block_on;
use object_store::{PagedDevice, PosIo};
use twizzler::{error::NamingError, Result};
use twizzler_abi::pager::PhysRange;
use twizzler_driver::{bus::pcie::PcieDeviceInfo, device::Device};

use super::MemDevice;
use crate::{helpers::PAGE, PAGER_CTX};

pub struct VirtioMem {
    device: Device,
    phys_start: u64,
    len: u64,
}

impl MemDevice for VirtioMem {
    fn get_physical(&self, start: u64, len: u64) -> Result<twizzler_abi::pager::PhysRange> {
        todo!()
    }

    fn page_size() -> u64
    where
        Self: Sized,
    {
        0x1000
    }

    fn flush(&self, start: u64, len: u64) -> Result<()> {
        Ok(())
    }
}

impl PosIo for VirtioMem {
    fn read(&self, start: u64, buf: &mut [u8]) -> Result<usize> {
        let queue = &PAGER_CTX.get().unwrap().sender;
        for (i, buf) in buf.chunks_mut(PAGE as usize).enumerate() {
            let start = self.phys_start + start + i as u64 * PAGE;
            let phys = PhysRange {
                start,
                end: start + PAGE,
            };
            block_on(crate::physrw::read_physical_pages(queue, buf, phys))?;
        }
        Ok(buf.len())
    }

    fn write(&self, start: u64, buf: &[u8]) -> Result<usize> {
        let queue = &PAGER_CTX.get().unwrap().sender;
        for (i, buf) in buf.chunks(PAGE as usize).enumerate() {
            let start = self.phys_start + start + i as u64 * PAGE;
            let phys = PhysRange {
                start,
                end: start + PAGE,
            };
            block_on(crate::physrw::fill_physical_pages(queue, buf, phys))?;
        }
        Ok(buf.len())
    }
}

impl PagedDevice for VirtioMem {
    fn sequential_read(&self, start: u64, list: &[object_store::PhysRange]) -> Result<usize> {
        Ok(0)
    }

    fn sequential_write(&self, start: u64, list: &[object_store::PhysRange]) -> Result<usize> {
        Ok(0)
    }

    fn len(&self) -> Result<usize> {
        Ok(self.len as usize)
    }

    fn phys_addrs(
        &self,
        start: Option<u64>,
        len: u64,
        _allow_failed_alloc: bool,
    ) -> Result<(object_store::PhysRange, bool)> {
        // TODO: bounds check
        Ok((
            PhysRange::new(
                start.unwrap() + self.phys_start,
                start.unwrap() + self.phys_start + len,
            ),
            true,
        ))
    }
}

pub async fn init_virtio() -> Result<VirtioMem> {
    let devices = devmgr::get_devices(devmgr::DriverSpec {
        supported: devmgr::Supported::Vendor(0x1af4, 0x105b),
    })?;

    for device in &devices {
        let device = Device::new(device.id).ok();
        if let Some(device) = device {
            let info = unsafe { device.get_info::<PcieDeviceInfo>(0).unwrap() };
            tracing::info!(
                "found virtio-mem controller at {:02x}:{:02x}.{:02x}",
                info.get_data().bus_nr,
                info.get_data().dev_nr,
                info.get_data().func_nr
            );

            let ctrl = VirtioMem {
                device,
                phys_start: 0,
                len: 0,
            };
            return Ok(ctrl);
        }
    }
    Err(NamingError::NotFound.into())
}
