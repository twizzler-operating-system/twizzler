use core::ptr::NonNull;

use twizzler_driver::dma::{Access, DmaOptions, DmaPool, DMA_PAGE_SIZE};
use virtio_drivers::{BufferDirection, Hal, PhysAddr};
use once_cell::sync::OnceCell;

pub struct TestHal;

static DMA_POOL_HOST_TO_DEVICE: OnceCell<DmaPool> = OnceCell::new();
static DMA_POOL_DEVICE_TO_HOST: OnceCell<DmaPool> = OnceCell::new();
static DMA_POOL_BIDIRECTIONAL: OnceCell<DmaPool> = OnceCell::new();

// Gets the global dma pool for the HAL. If it doesn't exist, creates a new one.
fn get_dma_pool(dir: BufferDirection) -> &'static DmaPool {
    match dir {
        BufferDirection::DriverToDevice => {
            match DMA_POOL_HOST_TO_DEVICE.get() {
                Some(pool) => pool,
                None => {
                    let pool = DmaPool::new(
                        DmaPool::default_spec(),
                        Access::HostToDevice,
                        DmaOptions::empty(),
                    );
                    DMA_POOL_HOST_TO_DEVICE.set(pool);
                    match DMA_POOL_HOST_TO_DEVICE.get() {
                        Some(pool) => pool,
                        None => panic!("Failed to set DMA_POOL_HOST_TO_DEVICE"),
                    }
                }
            }
        }
        BufferDirection::DeviceToDriver => {
            match DMA_POOL_DEVICE_TO_HOST.get() {
                Some(pool) => pool,
                None => {
                    let pool = DmaPool::new(
                        DmaPool::default_spec(),
                        Access::DeviceToHost,
                        DmaOptions::empty(),
                    );
                    DMA_POOL_DEVICE_TO_HOST.set(pool);
                    match DMA_POOL_DEVICE_TO_HOST.get() {
                        Some(pool) => pool,
                        None => panic!("Failed to set DMA_POOL_DEVICE_TO_HOST"),
                    }
                }
            }
        },
        BufferDirection::Both => {
            match DMA_POOL_BIDIRECTIONAL.get() {
                Some(pool) => pool,
                None => {
                    let pool = DmaPool::new(
                        DmaPool::default_spec(),
                        Access::BiDirectional,
                        DmaOptions::empty(),
                    );
                    DMA_POOL_BIDIRECTIONAL.set(pool);
                    match DMA_POOL_BIDIRECTIONAL.get() {
                        Some(pool) => pool,
                        None => panic!("Failed to set DMA_POOL_BIDIRECTIONAL"),
                    }
                }
            }
        },
    }
}

unsafe impl Hal for TestHal{
    // Required methods
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let alloced = get_dma_pool(direction).allocate_array(pages * DMA_PAGE_SIZE, 0);

        let mut dma_slice = alloced.unwrap();
        println!("Allocated DMA buffer of size: {}", dma_slice.num_bytes());
        let pin = dma_slice.pin().unwrap();
        
        assert_eq!(pin.len(), pages * DMA_PAGE_SIZE);
        let phys_addr: virtio_drivers::PhysAddr =
            u64::from(pin.into_iter().next().unwrap().addr()) as virtio_drivers::PhysAddr;

        let phys_copy = phys_addr;

        (phys_addr, NonNull::<u8>::new(phys_copy as *mut u8).unwrap())
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        //TODO: Implement this, the example program from virtio-drivers also just prints and
        // returns 0.
        0
    }

    unsafe fn mmio_phys_to_virt(_paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        panic!("Should never be called as we have our own transport implementation");
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        vaddr as PhysAddr
    }
    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}
