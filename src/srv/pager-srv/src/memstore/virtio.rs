use std::sync::Arc;

use async_io::block_on;
use object_store::{DevicePage, PagedDevice, PagedPhysMem, PhysRange, PosIo, PAGE_SIZE};
use twizzler::{
    error::{NamingError, ResourceError},
    Result,
};
use twizzler_driver::{bus::pcie::PcieDeviceInfo, device::Device, dma::PhysInfo};

use crate::{
    disk::SECTOR_SIZE, helpers::PAGE, physrw::register_phys, threads::run_async, PAGER_CTX,
};

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
    async fn read(&self, start: u64, mut buf: &mut [u8]) -> Result<usize> {
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
            crate::physrw::read_physical_pages(&mut read_buffer, phys).await?;

            let bytes_to_read = right - left;
            buf[bytes_written..bytes_written + bytes_to_read]
                .copy_from_slice(&read_buffer[left..right]);

            bytes_written += bytes_to_read;
            pos += bytes_to_read;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_written)
    }

    async fn write(&self, start: u64, mut buf: &[u8]) -> Result<usize> {
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
                self.read(temp_pos & !(PAGE_SIZE - 1) as u64, &mut write_buffer)
                    .await?;
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            pos += right - left;

            let start = self.phys_start + (lba * SECTOR_SIZE) as u64;
            let phys = PhysRange {
                start,
                end: start + write_buffer.len() as u64,
            };
            crate::physrw::fill_physical_pages(&write_buffer, phys).await?;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
    }
}

impl PagedDevice for VirtioMem {
    async fn sequential_read(&self, start: u64, list: &[object_store::PhysRange]) -> Result<usize> {
        tracing::warn!("seq-read on virtio-mem");
        Ok(0)
    }

    async fn sequential_write(
        &self,
        start: u64,
        list: &[object_store::PhysRange],
    ) -> Result<usize> {
        tracing::warn!("seq-write on virtio-mem");
        Ok(list.len())
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.len as usize)
    }

    async fn phys_addrs(
        &self,
        start: DevicePage,
        phys_list: &mut Vec<PagedPhysMem>,
    ) -> Result<usize> {
        // TODO: bounds check
        let alloc_page = || {
            let ctx = PAGER_CTX.get().unwrap();
            let page = match ctx.data.try_alloc_page() {
                Ok(page) => page,
                Err(mw) => {
                    tracing::debug!("OOM: (ok = {})", !phys_list.is_empty());
                    if !phys_list.is_empty() {
                        return None;
                    }
                    run_async(mw)
                }
            };
            let phys_range = PhysRange::new(page, page + PAGE);
            Some(phys_range)
        };
        let (start, len) = match start {
            DevicePage::Run(start, len) => (start, len as u64),
            DevicePage::Hole(_len) => {
                let page = alloc_page();
                if let Some(page) = page {
                    phys_list.push(PagedPhysMem::new(page).completed());
                    return Ok(1);
                } else {
                    return Ok(0);
                }
            }
        };
        let phys_range = PhysRange::new(
            start * PAGE + self.phys_start,
            start * PAGE + self.phys_start + len * PAGE,
        );
        phys_list.push(PagedPhysMem::new(phys_range).completed().wired());
        Ok(len as usize)
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
            if register_phys(start, len).await.is_ok() {
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
