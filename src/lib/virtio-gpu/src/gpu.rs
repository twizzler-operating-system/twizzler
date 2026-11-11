use std::sync::{Arc, Mutex};

use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_driver::dma::{Access, DmaObject, DmaOptions, DmaPool, DmaSliceRegion, DMA_PAGE_SIZE};
use virtio_drivers::{
    device::gpu::VirtIOGpu,
    transport::{pci::VirtioPciError, Transport},
    Error,
};

use crate::{
    hal::{TwzHal, MAX_CONTIGUOUS_PAGES},
    transport::TwizzlerTransport,
};

type DeviceImpl<T> = VirtIOGpu<TwzHal, T>;

#[derive(Clone)]
pub struct DeviceWrapper<T: Transport> {
    inner: Arc<Mutex<DeviceImpl<T>>>,
    framebuffer: Arc<Mutex<Vec<DmaSliceRegion<u8>>>>,
}

impl<T: Transport> DeviceWrapper<T> {
    fn new(dev: DeviceImpl<T>) -> Self {
        DeviceWrapper {
            inner: Arc::new(Mutex::new(dev)),
            framebuffer: Arc::default(),
        }
    }

    /// Allocates the framebuffer and attaches it to the scanout. It is contiguous in virtual
    /// memory only: physically it is a run per `MAX_CONTIGUOUS_PAGES`, each attached separately.
    pub fn setup_framebuffer(&self) -> Result<(*mut u8, usize), Error> {
        const CHUNK: usize = MAX_CONTIGUOUS_PAGES * DMA_PAGE_SIZE;
        let mut gpu = self.inner.lock().unwrap();
        let (width, height) = gpu.resolution()?;
        let len = (width * height * 4) as usize;
        let obj = DmaPool::default_spec()
            .build(())
            .map_err(|_| Error::DmaError)?;
        let obj = DmaObject::new(obj);

        let mut regions = Vec::new();
        let mut runs: Vec<(u64, u32)> = Vec::new();
        for offset in (0..len).step_by(CHUNK) {
            let chunk = CHUNK.min(len - offset);
            let mut region = obj.slice_region::<u8>(
                NULLPAGE_SIZE + offset,
                chunk,
                Access::HostToDevice,
                DmaOptions::CONTIGUOUS,
            );
            let addr = u64::from(region.pin().map_err(|_| Error::DmaError)?[0].addr());
            match runs.last_mut() {
                Some((start, length)) if *start + u64::from(*length) == addr => {
                    *length += chunk as u32
                }
                _ => runs.push((addr, chunk as u32)),
            }
            regions.push(region);
        }

        // SAFETY: the regions hold the pins, and are kept for as long as the device is.
        unsafe { gpu.setup_framebuffer_with_backing(&runs)? };
        let virt = unsafe { regions[0].get_mut().as_mut_ptr() };
        *self.framebuffer.lock().unwrap() = regions;
        Ok((virt, len))
    }

    pub fn with_device<R>(&self, f: impl FnOnce(&mut DeviceImpl<T>) -> R) -> R {
        f(&mut *self.inner.lock().unwrap())
    }
}

// Gets the Virtio Net struct which implements the device used for smoltcp. Use this to create a
// smoltcp interface to send and receive packets. NOTE: Only the first device used will work
// properly
pub fn get_device(
    notifier: std::sync::mpsc::Sender<Option<()>>,
) -> Result<DeviceWrapper<TwizzlerTransport>, VirtioPciError> {
    let gpu = VirtIOGpu::<TwzHal, TwizzlerTransport>::new(TwizzlerTransport::new(notifier)?)
        .expect("failed to create gpu driver");
    Ok(DeviceWrapper::<TwizzlerTransport>::new(gpu))
}
