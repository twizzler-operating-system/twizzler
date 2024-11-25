use twizzler_abi::syscall::{BackingType, LifetimeType};
use twizzler_abi::marker::{BaseType, BaseVersion, BaseTag};
use twizzler_driver::dma::{Access, DmaOptions, DmaPool, DmaSliceRegion, DMA_PAGE_SIZE};
use twizzler_object::{CreateSpec, Object};

use virtio_drivers::{BufferDirection, Hal, PhysAddr};

use once_cell::sync::OnceCell; 
use std::cell::UnsafeCell;
use std::collections::HashMap;
use fragile::Fragile;
use std::sync::Mutex;
use core::ptr::NonNull;

pub struct TestHal;

static DMA_POOL_HOST_TO_DEVICE: OnceCell<DmaPool> = OnceCell::new();
static DMA_POOL_DEVICE_TO_HOST: OnceCell<DmaPool> = OnceCell::new();
static DMA_POOL_BIDIRECTIONAL: OnceCell<DmaPool> = OnceCell::new();

// The DmaSliceRegions contained within this hashmap are never operated upon after being inserted into this hashmap, only the memory beneath it is.
// This hashmap is used to keep memory allocated while it is still in use.
static ALLOCED: OnceCell<Mutex<HashMap<PhysAddr, Fragile<DmaSliceRegion<u8>>>>> = OnceCell::new();

static BUFFERS: OnceCell<Mutex<HashMap<PhysAddr, Object<BufferWrapper>>>> = OnceCell::new();

struct BufferWrapper {
    buffer: NonNull<[u8]>
}

unsafe impl Send for BufferWrapper {}

impl BaseType for BufferWrapper {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(BaseVersion, BaseTag)] {
        todo!()
    }
}

// Gets the global dma pool for the HAL in a given access direction. If it doesn't exist, create it.
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

fn insert_alloced(paddr: PhysAddr, dma_slice: DmaSliceRegion<u8>) {
    let dict = ALLOCED.get_or_init(|| Mutex::new(HashMap::new()));
    
    let wrapped = Fragile::new(dma_slice);

    dict.lock().unwrap().insert(paddr, wrapped);
}

fn remove_alloced(paddr: PhysAddr) -> Option<Fragile<DmaSliceRegion<u8>>> {
    let dict = ALLOCED.get_or_init(|| Mutex::new(HashMap::new()));

    dict.lock().unwrap().remove(&paddr)
}

unsafe impl Hal for TestHal{
    // Required methods
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        assert!(pages == 1, "Only 1 page supported");

        let alloced = get_dma_pool(direction).allocate_array(pages * DMA_PAGE_SIZE, 0u8);

        let mut dma_slice = alloced.unwrap();
        let pin = dma_slice.pin().unwrap();
        
        let phys_addr: virtio_drivers::PhysAddr =
            u64::from(pin.into_iter().next().unwrap().addr()) as virtio_drivers::PhysAddr;

        let ptr = unsafe{dma_slice.get_mut().as_mut_ptr()};

        // Persist the allocated memory so it isn't freed when the function returns
        insert_alloced(phys_addr, dma_slice);

        println!("Allocated DMA buffer at: {:?} with phys addr: {:x}", ptr, phys_addr);

        (phys_addr, NonNull::<u8>::new(ptr).unwrap())
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        remove_alloced(_paddr);
        0
    }

    unsafe fn mmio_phys_to_virt(_paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        panic!("Should never be called as we have our own transport implementation");
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        // let vaddr = buffer.as_ptr() as *mut u8 as usize;
        // println!("Sharing buffer at {:?}", buffer);
        // vaddr as PhysAddr
        
        let create_spec = &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal);
        
        let obj: Object<BufferWrapper> = Object::create_with(create_spec, |obj| unsafe {
            let base: &mut BufferWrapper = obj.base_mut_unchecked().assume_init_mut();
            base.buffer.copy_from(buffer, buffer.len());
        }).unwrap(); 



        // Pin object and get phys address
        
        
        // Hold onto the object until it's unshared.
        let dict = BUFFERS.get_or_init(|| Mutex::new(HashMap::new()));
        todo!();
    }
    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        todo!();
        //Find the associated object
        
        
        // Copy the buffer back to the original buffer


        // Unpin object
    }
}
