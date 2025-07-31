use std::sync::Arc;

use async_io::block_on;
use object_store::{PagedDevice, PosIo, PAGE_SIZE};
use twizzler::{
    error::{NamingError, ResourceError},
    Result,
};
use twizzler_abi::pager::PhysRange;
use twizzler_driver::{bus::pcie::PcieDeviceInfo, device::Device};

use crate::{disk::SECTOR_SIZE, helpers::PAGE, physrw::register_phys, PAGER_CTX};

#[derive(Clone)]
pub struct VirtioMem {
    device: Arc<Device>,
    phys_start: u64,
    len: u64,
}

impl VirtioMem {
    fn lba_count(&self) -> usize {
        self.len as usize / SECTOR_SIZE
    }
}

impl PosIo for VirtioMem {
    fn read(&self, start: u64, mut buf: &mut [u8]) -> Result<usize> {
        let queue = &PAGER_CTX.get().unwrap().sender;
        let mut pos = start as usize;
        let mut lba = (pos / PAGE_SIZE) * 8;
        let mut bytes_written: usize = 0;
        let mut read_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_written != buf.len() {
            if lba >= self.lba_count() {
                break;
            }

            let left = pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_written > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_written
            }; // If I want to write more than the boundary of a page

            let start = self.phys_start + (lba * SECTOR_SIZE) as u64;
            let phys = PhysRange {
                start,
                end: start + read_buffer.len() as u64,
            };
            block_on(crate::physrw::read_physical_pages(
                queue,
                &mut read_buffer,
                phys,
            ))?;

            let bytes_to_read = right - left;
            buf[bytes_written..bytes_written + bytes_to_read]
                .copy_from_slice(&read_buffer[left..right]);

            bytes_written += bytes_to_read;
            pos += bytes_to_read;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_written)
    }

    fn write(&self, start: u64, mut buf: &[u8]) -> Result<usize> {
        let queue = &PAGER_CTX.get().unwrap().sender;
        let mut pos = start as usize;
        let mut lba = (pos / PAGE_SIZE) * 8;
        let mut bytes_read = 0;
        let mut write_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_read != buf.len() {
            if lba >= self.lba_count() {
                break;
            }

            let left = pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_read > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_read
            };
            if right - left != PAGE_SIZE {
                let temp_pos: u64 = pos.try_into().unwrap();
                // TODO: check if full read
                self.read(temp_pos & !(PAGE_SIZE - 1) as u64, &mut write_buffer)?;
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            pos += right - left;

            let start = self.phys_start + (lba * SECTOR_SIZE) as u64;
            let phys = PhysRange {
                start,
                end: start + write_buffer.len() as u64,
            };
            block_on(crate::physrw::fill_physical_pages(
                queue,
                &write_buffer,
                phys,
            ))?;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
    }
}

impl PagedDevice for VirtioMem {
    fn sequential_read(&self, start: u64, list: &[object_store::PhysRange]) -> Result<usize> {
        tracing::warn!("seq-read on virtio-mem");
        Ok(0)
    }

    fn sequential_write(&self, start: u64, list: &[object_store::PhysRange]) -> Result<usize> {
        tracing::warn!("seq-write on virtio-mem");
        Ok(list.len())
    }

    fn len(&self) -> Result<usize> {
        Ok(self.len as usize)
    }

    fn phys_addrs(
        &self,
        start: Option<u64>,
        len: u64,
        allow_failed_alloc: bool,
    ) -> Result<(object_store::PhysRange, bool)> {
        // TODO: bounds check
        let alloc_page = |completed: bool| {
            let ctx = PAGER_CTX.get().unwrap();
            let page = match ctx.data.try_alloc_page() {
                Ok(page) => page,
                Err(mw) => {
                    tracing::debug!("OOM: (ok = {})", allow_failed_alloc);
                    if allow_failed_alloc {
                        return Err(ResourceError::OutOfMemory.into());
                    }
                    block_on(mw)
                }
            };
            let phys_range = PhysRange::new(page, page + PAGE);
            Ok((phys_range, completed))
        };
        let Some(start) = start else {
            return alloc_page(false);
        };
        if start == 0 {
            return alloc_page(true);
        }
        Ok((
            PhysRange::new(
                start * PAGE + self.phys_start,
                start * PAGE + self.phys_start + len,
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
            let bar = device.get_mmio(1).unwrap();

            let start = unsafe { bar.get_mmio_offset::<u64>(0x2000) }
                .as_ptr()
                .read();
            let len = unsafe { bar.get_mmio_offset::<u64>(0x2008) }
                .as_ptr()
                .read();

            tracing::info!("virtio-mem start at {:x} len: {:x}", start, len);
            if register_phys(&PAGER_CTX.get().unwrap().sender, start, len)
                .await
                .is_ok()
            {
                tracing::info!("virtio-mem registered physical region with kernel",);
                let ctrl = VirtioMem {
                    device: Arc::new(device),
                    phys_start: start,
                    len,
                };
                return Ok(ctrl);
            }
        }
    }
    Err(NamingError::NotFound.into())
}
