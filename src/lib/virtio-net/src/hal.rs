use core::ptr::NonNull;
use std::{collections::HashMap, ptr::copy_nonoverlapping, sync::Mutex};

use once_cell::sync::OnceCell;
use twizzler_driver::dma::{Access, DmaOptions, DmaPool, DmaSliceRegion, SyncMode, DMA_PAGE_SIZE};
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

pub struct TestHal;

static DMA_POOL_HOST_TO_DEVICE: OnceCell<DmaPool> = OnceCell::new();
static DMA_POOL_DEVICE_TO_HOST: OnceCell<DmaPool> = OnceCell::new();
static DMA_POOL_BIDIRECTIONAL: OnceCell<DmaPool> = OnceCell::new();

// The DmaSliceRegions contained within this hashmap are never operated upon after being inserted
// into this hashmap, only the memory beneath it is. This hashmap is used to keep memory allocated
// while it is still in use. Fragile type only allows the thread that original thread that created
// the object to call its destructor.
static ALLOCED: OnceCell<Mutex<HashMap<PhysAddr, DmaSliceRegion<u8>>>> = OnceCell::new();

// Gets the global dma pool for the HAL in a given access direction. If it doesn't exist, create it.
fn get_dma_pool(dir: BufferDirection) -> &'static DmaPool {
    match dir {
        BufferDirection::DriverToDevice => match DMA_POOL_HOST_TO_DEVICE.get() {
            Some(pool) => pool,
            None => {
                let pool = DmaPool::new(
                    DmaPool::default_spec(),
                    Access::HostToDevice,
                    DmaOptions::empty(),
                );
                match DMA_POOL_HOST_TO_DEVICE.set(pool) {
                    Ok(_) => {}
                    Err(_) => panic!("Failed to set DMA_POOL_HOST_TO_DEVICE"),
                }
                match DMA_POOL_HOST_TO_DEVICE.get() {
                    Some(pool) => pool,
                    None => panic!("Failed to set DMA_POOL_HOST_TO_DEVICE"),
                }
            }
        },
        BufferDirection::DeviceToDriver => match DMA_POOL_DEVICE_TO_HOST.get() {
            Some(pool) => pool,
            None => {
                let pool = DmaPool::new(
                    DmaPool::default_spec(),
                    Access::DeviceToHost,
                    DmaOptions::empty(),
                );
                match DMA_POOL_DEVICE_TO_HOST.set(pool) {
                    Ok(_) => {}
                    Err(_) => panic!("Failed to set DMA_POOL_DEVICE_TO_HOST"),
                };
                match DMA_POOL_DEVICE_TO_HOST.get() {
                    Some(pool) => pool,
                    None => panic!("Failed to set DMA_POOL_DEVICE_TO_HOST"),
                }
            }
        },
        BufferDirection::Both => match DMA_POOL_BIDIRECTIONAL.get() {
            Some(pool) => pool,
            None => {
                let pool = DmaPool::new(
                    DmaPool::default_spec(),
                    Access::BiDirectional,
                    DmaOptions::empty(),
                );
                match DMA_POOL_BIDIRECTIONAL.set(pool) {
                    Ok(_) => {}
                    Err(_) => panic!("Failed to set DMA_POOL_BIDIRECTIONAL"),
                };
                match DMA_POOL_BIDIRECTIONAL.get() {
                    Some(pool) => pool,
                    None => panic!("Failed to set DMA_POOL_BIDIRECTIONAL"),
                }
            }
        },
    }
}

fn insert_alloced(paddr: PhysAddr, dma_slice: DmaSliceRegion<u8>) {
    let dict = ALLOCED.get_or_init(|| Mutex::new(HashMap::new()));
    dict.lock().unwrap().insert(paddr, dma_slice);
}

fn remove_alloced(paddr: PhysAddr) -> Option<DmaSliceRegion<u8>> {
    let dict = ALLOCED.get_or_init(|| Mutex::new(HashMap::new()));
    dict.lock().unwrap().remove(&paddr)
}

unsafe impl Hal for TestHal {
    // Required methods
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        assert!(pages == 1, "Only 1 page supported");

        let pool = get_dma_pool(direction);
        let alloced = pool.allocate_array(pages, 0u8).unwrap();
        let mut dma_slice = alloced;

        let pin = dma_slice.pin().unwrap();
        let phys_addr: virtio_drivers::PhysAddr =
            u64::from(pin.into_iter().next().unwrap().addr()) as virtio_drivers::PhysAddr;
        let virt = unsafe { NonNull::<u8>::new(dma_slice.get_mut().as_mut_ptr()) }.unwrap();

        // Persist the allocated memory so it isn't freed when the function returns
        insert_alloced(phys_addr, dma_slice);
        (phys_addr as PhysAddr, virt)
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        let mut dma_region = remove_alloced(_paddr).unwrap();
        dma_region.release_pin();
        0
    }

    unsafe fn mmio_phys_to_virt(_paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        panic!("Should never be called as we have our own transport implementation");
    }

    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
        let buf_len = buffer.len();
        assert!(buf_len <= DMA_PAGE_SIZE, "Hal::Share(): Buffer too large");
        let (phys, virt) = TestHal::dma_alloc(1, direction);
        let slice = remove_alloced(phys).unwrap();

        let buf_casted = buffer.cast::<u8>();
        let buf = buf_casted.as_ptr();
        let dma_buf = virt.as_ptr();
        // Copy the buffer to the DMA buffer
        copy_nonoverlapping(buf, dma_buf, buf_len);

        match direction {
            BufferDirection::DriverToDevice => {
                slice.sync(0..buf_len, SyncMode::PostCpuToDevice);
            }
            BufferDirection::DeviceToDriver => {
                slice.sync(0..buf_len, SyncMode::PreDeviceToCpu);
            }
            _ => {}
        }
        // Persist the allocated memory so it isn't freed when the function returns
        insert_alloced(phys, slice);
        phys as PhysAddr
    }
    unsafe fn unshare(paddr: PhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {
        // Gets DMA buffer and unallocates it
        let mut dma_slice = remove_alloced(paddr).unwrap();
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
        dma_slice.release_pin();
    }
}
