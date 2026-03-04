use core::ptr::NonNull;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use twizzler::object::Object;
use twizzler_driver::dma::{Access, DmaObject, DmaOptions, DmaPool, DmaSliceRegion, DMA_PAGE_SIZE};
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

struct TwzHalStatic {
    host_to_device: DmaPool,
    device_to_host: DmaPool,
    bidirectional: DmaPool,

    dma_slices: HashMap<usize, DmaSliceRegion<u8>>,
    available: Vec<DmaSliceRegion<u8>>,
    shared: HashMap<PhysAddr, DmaSliceRegion<u8>>,
}

pub struct TwzHal;

static TWZHAL: OnceLock<Mutex<TwzHalStatic>> = OnceLock::new();

fn get_twz_hal() -> &'static Mutex<TwzHalStatic> {
    TWZHAL.get_or_init(|| Mutex::new(TwzHalStatic::new()))
}

// Gets the global dma pool for the HAL in a given access direction. If it doesn't exist, create it.
impl TwzHalStatic {
    fn new() -> Self {
        Self {
            host_to_device: DmaPool::new(
                DmaPool::default_spec(),
                Access::HostToDevice,
                DmaOptions::empty(),
            ),
            device_to_host: DmaPool::new(
                DmaPool::default_spec(),
                Access::DeviceToHost,
                DmaOptions::empty(),
            ),
            bidirectional: DmaPool::new(
                DmaPool::default_spec(),
                Access::BiDirectional,
                DmaOptions::empty(),
            ),
            available: Vec::new(),
            shared: HashMap::new(),
            dma_slices: HashMap::new(),
        }
    }

    fn get_dma_pool(&self, dir: BufferDirection) -> &DmaPool {
        match dir {
            BufferDirection::DriverToDevice => &self.host_to_device,
            BufferDirection::DeviceToDriver => &self.device_to_host,
            BufferDirection::Both => &self.bidirectional,
        }
    }
}

unsafe impl Hal for TwzHal {
    // Required methods
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        assert!(pages == 1, "Only 1 page supported");

        let mut twzhal = get_twz_hal().lock().unwrap();
        let mut dma_slice = if let Some(buffer) = twzhal.available.pop() {
            buffer
        } else {
            let pool = twzhal.get_dma_pool(direction);
            pool.allocate_array(pages, 0u8).unwrap()
        };

        let pin = dma_slice.pin().unwrap();
        let phys_addr: virtio_drivers::PhysAddr =
            u64::from(pin.into_iter().next().unwrap().addr()) as virtio_drivers::PhysAddr;
        let virt = unsafe { NonNull::<u8>::new(dma_slice.get_mut().as_mut_ptr()) }.unwrap();
        twzhal.shared.insert(phys_addr, dma_slice);
        (phys_addr as PhysAddr, virt)
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        //tracing::info!("DEALLOC: {:?} {:p}", paddr, _vaddr);
        let mut twzhal = get_twz_hal().lock().unwrap();
        if let Some(dma_slice) = twzhal.shared.remove(&paddr) {
            twzhal.available.push(dma_slice);
        }
        0
    }

    unsafe fn mmio_phys_to_virt(_paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        panic!("Should never be called as we have our own transport implementation");
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        let buf_len = buffer.len();
        //tracing::info!("SHARE: {:p} {} {:?}", buffer, buf_len, direction);
        assert!(buf_len <= DMA_PAGE_SIZE, "Hal::Share(): Buffer too large");

        let addr: usize = buffer.addr().into();
        let page_align_addr = addr & !(DMA_PAGE_SIZE - 1);
        let page_offset = addr - page_align_addr;

        let mut twzhal = TWZHAL.get().unwrap().lock().unwrap();
        let entry = twzhal.dma_slices.entry(page_align_addr).or_insert_with(|| {
            tracing::debug!("setting up DMA for object, ptr = {:p}", buffer);
            let handle =
                twizzler_rt_abi::object::twz_rt_get_object_handle(buffer.as_ptr().cast::<u8>())
                    .unwrap();
            let offset = page_align_addr - handle.start().addr();
            let obj = DmaObject::new(Object::<()>::from_handle(handle).unwrap());
            obj.slice_region(
                offset,
                DMA_PAGE_SIZE,
                Access::BiDirectional,
                DmaOptions::empty(),
            )
        });

        let pin = entry.pin().unwrap();
        let phys = pin.backing[0].addr();

        (phys.0 + page_offset as u64) as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        //tracing::info!("UNSHARE: {:?} {:p} {:?}", paddr, buffer, direction);
        // Gets DMA buffer and unallocates it
        /*
        let mut twzhal = get_twz_hal().lock().unwrap();
        if let Some(mut dma_slice) = twzhal.shared.remove(&paddr) {
            match direction {
                BufferDirection::DeviceToDriver => {
                    dma_slice.sync(0..buffer.len(), SyncMode::PostDeviceToCpu);
                }
                _ => {}
            }

            let buf_len = buffer.len();
            let buf_casted = buffer.cast::<u8>();
            let buf = buf_casted.as_ptr();
            let dma_buf = unsafe { dma_slice.get_mut().as_ptr() };

            // Copy the DMA buffer back to the buffer
            copy_nonoverlapping(dma_buf, buf, buf_len);
            twzhal.available.push(dma_slice);
        }
        */
    }
}
