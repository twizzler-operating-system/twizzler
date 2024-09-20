use core::ptr::NonNull;
use volatile::VolatilePtr;

use twizzler_driver::{bus::pcie::PcieDeviceHeader, dma::{Access, DmaOptions, DmaPool, DMA_PAGE_SIZE}};
use virtio_drivers::{BufferDirection, Hal, PhysAddr};
use twizzler_driver::bus::pcie::{PcieDeviceInfo, PcieFunctionHeader};
use twizzler_abi::device::bus::pcie::get_bar;

pub struct TestHal<'a> {
    dma_pool_host_to_device: DmaPool,
    dma_pool_device_to_host: DmaPool,
    dma_pool_bi_directional: DmaPool,

    pcie_config: VolatilePtr<'a, PcieDeviceHeader>,
}

impl TestHal {
    pub fn new(cfg: VolatilePtr<'a, PcieDeviceHeader>) -> Self{
        Self {
            dma_pool_host_to_device: DmaPool::new(
                DmaPool::default_spec(),
                Access::HostToDevice,
                DmaOptions::empty(),
            ),
            dma_pool_device_to_host: DmaPool::new(
                DmaPool::default_spec(),
                Access::DeviceToHost,
                DmaOptions::empty(),
            ),
            dma_pool_bi_directional: DmaPool::new(
                DmaPool::default_spec(),
                Access::BiDirectional,
                DmaOptions::empty(),
            ),
            pcie_config: cfg,
        }
    }
}


unsafe impl <'a>Hal for TestHal<'a> {
    // Required methods
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        // Probably should just make this not create a new DmaPool every time
        let alloced = match direction {
            BufferDirection::DriverToDevice => DmaPool::new(
                DmaPool::default_spec(),
                Access::HostToDevice,
                DmaOptions::empty(),
            )
            .allocate_array(pages * DMA_PAGE_SIZE, 0),
            BufferDirection::DeviceToDriver => DmaPool::new(
                DmaPool::default_spec(),
                Access::DeviceToHost,
                DmaOptions::empty(),
            )
            .allocate_array(pages * DMA_PAGE_SIZE, 0),
            BufferDirection::Both => DmaPool::new(
                DmaPool::default_spec(),
                Access::BiDirectional,
                DmaOptions::empty(),
            )
            .allocate_array(pages * DMA_PAGE_SIZE, 0),
        };
        let mut dma_slice = alloced.unwrap();
        let pin = dma_slice.pin().unwrap();

        assert_eq!(pin.len(), pages * DMA_PAGE_SIZE);
        let phys_addr: virtio_drivers::PhysAddr =
            u64::from(pin.into_iter().next().unwrap().addr()) as virtio_drivers::PhysAddr;

        let phys_copy = phys_addr;

        (phys_addr, NonNull::<u8>::new(phys_copy as *mut u8).unwrap())
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        //TODO: Implement this, the example program from virtio-drivers also just prints and
        // returns 0.
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8> {
        get_bar(pcie_cfg, size).as_raw_ptr().as_ptr()
    }

    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
        let vaddr = buffer.as_ptr() as *mut u8 as usize;
        vaddr as PhysAddr
    }
    unsafe fn unshare(paddr: PhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {}
}
