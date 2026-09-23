use core::ops::Deref;

/// A reference to a memory mapped IO region of
/// memory of type `T`
pub struct MmioRef<T> {
    address: *const T,
}

impl<T> MmioRef<T> {
    pub fn new(address: *const T) -> Self {
        Self { address }
    }
}

impl<T> Deref for MmioRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.address }
    }
}

unsafe impl<T> Send for MmioRef<T> {}
unsafe impl<T> Sync for MmioRef<T> {}

/// Map `len` bytes of device memory at `phys` into the kernel's MMIO range.
pub(crate) fn map_device_region(
    phys: crate::memory::PhysAddr,
    len: usize,
) -> crate::memory::VirtAddr {
    use twizzler_abi::{device::CacheType, object::Protections};

    use crate::{
        arch::{context::ArchContextTarget, memory::mmio::mmio_allocator},
        memory::{
            frame::PHYS_LEVEL_LAYOUTS,
            pagetables::{
                Consistency, ContiguousProvider, Mapper, MappingCursor, MappingFlags,
                MappingSettings,
            },
            tracker::{FrameAllocFlags, FrameAllocator},
        },
    };

    let va = mmio_allocator()
        .lock()
        .alloc(len)
        .expect("failed to allocate MMIO region");
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::MemoryMappedIO,
        MappingFlags::GLOBAL,
    );
    let mut provider = ContiguousProvider::new(phys, len, settings);
    unsafe {
        let mut mapper = Mapper::current();
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut consist = Consistency::new(ArchContextTarget(mapper.root_address()));
        mapper
            .map(
                MappingCursor::new(va, len),
                &mut provider,
                &mut consist,
                &mut fa,
            )
            .unwrap();
        consist.tlb_mut().finish();
        consist.into_deferred().run_all();
    }
    va
}
