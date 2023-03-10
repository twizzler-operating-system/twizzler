use core::{fmt::LowerHex, ops::Sub};

use crate::once::Once;

use super::memory::phys_to_virt;

/// A representation of a canonical virtual address.
#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
pub struct VirtAddr(u64);

/// A representation of a valid physical address.
#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
pub struct PhysAddr(u64);

#[derive(Debug, Clone, Copy)]
pub struct NonCanonical;

impl VirtAddr {
    /// The start of the kernel memory heap.
    pub const HEAP_START: Self = Self(0xffffff0000000000);
    /// The start of the kernel object mapping.
    const KOBJ_START: Self = Self(0xfffff00000000000);
    /// The start of the physical mapping.
    const PHYS_START: Self = Self(0xffff800000000000);

    pub const fn start_kernel_memory() -> Self {
        // This is defined by the definitions of the two canonical regions of the virtual memory space.
        Self(0xffff800000000000)
    }

    pub const fn start_kernel_object_memory() -> Self {
        Self::KOBJ_START
    }

    pub const fn end_kernel_object_memory() -> Self {
        Self::HEAP_START
    }

    pub const fn start_user_memory() -> Self {
        // This is defined by the definitions of the two canonical regions of the virtual memory space.
        Self(0x0)
    }

    pub const fn end_user_memory() -> Self {
        // This is defined by the definitions of the two canonical regions of the virtual memory space.
        Self(0x0000800000000000)
    }

    /// Construct a new virtual address from the provided addr value, only if the provided value is a valid, canonical
    /// address. If not, returns Err.
    pub const fn new(addr: u64) -> Result<Self, NonCanonical> {
        // This is defined by the definitions of the two canonical regions of the virtual memory space.
        if addr >= 0xFFFF800000000000 || addr <= 0x00007fffffffffff {
            Ok(Self(addr))
        } else {
            Err(NonCanonical)
        }
    }

    /// Construct a new virtual address from a u64 without verifying that it is a valid virtual address.
    ///
    /// # Safety
    /// The provided address must be canonical.
    pub const unsafe fn new_unchecked(addr: u64) -> Self {
        Self(addr)
    }

    pub fn as_mut_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }

    pub fn as_ptr<T>(&self) -> *const T {
        self.0 as *const T
    }

    pub fn is_aligned_to(&self, alignment: usize) -> bool {
        self.0 % alignment as u64 == 0
    }

    pub fn is_kernel(&self) -> bool {
        // This is defined by the definitions of the two canonical regions of the virtual memory space.
        self.0 >= 0xffff800000000000
    }

    pub fn is_kernel_object_memory(&self) -> bool {
        self.0 >= Self::start_kernel_object_memory().0
            && self.0 < Self::end_kernel_object_memory().0
    }

    pub fn offset<U: Into<Offset>>(&self, offset: U) -> Result<Self, NonCanonical> {
        let offset = offset.into();
        match offset {
            Offset::Usize(u) => Self::new(self.0.checked_add(u as u64).ok_or(NonCanonical)?),
            Offset::Isize(u) => {
                let abs = u.abs();
                if u < 0 {
                    Self::new(self.0.checked_sub(abs as u64).ok_or(NonCanonical)?)
                } else {
                    Self::new(self.0.checked_add(abs as u64).ok_or(NonCanonical)?)
                }
            }
        }
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn from_ptr<T>(ptr: *const T) -> Self {
        Self(ptr as u64)
    }

    pub fn align_down<U: Into<u64>>(&self, align: U) -> Result<Self, NonCanonical> {
        let align = align.into();
        assert!(align.is_power_of_two(), "`align` must be a power of two");
        Self::new(self.raw() & !(align - 1))
    }

    pub fn align_up<U: Into<u64>>(&self, align: U) -> Result<Self, NonCanonical> {
        let align = align.into();
        assert!(align.is_power_of_two(), "`align` must be a power of two");
        let mask = align - 1;
        if self.raw() & mask == 0 {
            Ok(*self)
        } else if let Some(aligned) = (self.raw() | mask).checked_add(1) {
            Self::new(aligned)
        } else {
            Err(NonCanonical)
        }
    }
}

impl<T> From<*mut T> for VirtAddr {
    fn from(x: *mut T) -> Self {
        Self(x as usize as u64)
    }
}

impl<T> From<*const T> for VirtAddr {
    fn from(x: *const T) -> Self {
        Self(x as usize as u64)
    }
}

impl TryFrom<u64> for VirtAddr {
    type Error = NonCanonical;

    fn try_from(addr: u64) -> Result<Self, Self::Error> {
        Self::new(addr)
    }
}

impl TryFrom<usize> for VirtAddr {
    type Error = NonCanonical;

    fn try_from(addr: usize) -> Result<Self, Self::Error> {
        Self::new(addr as u64)
    }
}

impl From<VirtAddr> for u64 {
    fn from(addr: VirtAddr) -> Self {
        addr.0
    }
}

impl From<VirtAddr> for usize {
    fn from(addr: VirtAddr) -> Self {
        addr.0 as usize
    }
}

static PHYS_ADDR_WIDTH: Once<u64> = Once::new();
impl PhysAddr {
    fn get_phys_addr_width() -> u64 {
        *PHYS_ADDR_WIDTH.call_once(|| {
            x86::cpuid::CpuId::new()
                .get_processor_capacity_feature_info()
                .unwrap()
                .physical_address_bits()
                .into()
        })
    }

    pub fn new(addr: u64) -> Result<Self, NonCanonical> {
        let bits = Self::get_phys_addr_width();
        if bits == 64 || addr < 1 << bits {
            Ok(Self(addr))
        } else {
            Err(NonCanonical)
        }
    }

    /// Construct a new physical address from a u64 without verifying that it is a valid physical address.
    ///
    /// # Safety
    /// The provided address must be a valid address.
    pub unsafe fn new_unchecked(addr: u64) -> Self {
        Self(addr)
    }

    pub fn kernel_vaddr(&self) -> VirtAddr {
        phys_to_virt(*self)
    }

    pub fn offset<U: Into<Offset>>(&self, offset: U) -> Result<Self, NonCanonical> {
        let offset = offset.into();
        match offset {
            Offset::Usize(u) => Self::new(self.0.checked_add(u as u64).ok_or(NonCanonical)?),
            Offset::Isize(u) => {
                let abs = u.abs();
                if u < 0 {
                    Self::new(self.0.checked_sub(abs as u64).ok_or(NonCanonical)?)
                } else {
                    Self::new(self.0.checked_add(abs as u64).ok_or(NonCanonical)?)
                }
            }
        }
    }

    pub fn is_aligned_to(&self, alignment: usize) -> bool {
        self.0 % alignment as u64 == 0
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn align_down<U: Into<u64>>(&self, align: U) -> Result<Self, NonCanonical> {
        let align = align.into();
        assert!(align.is_power_of_two(), "`align` must be a power of two");
        Self::new(self.raw() & !(align - 1))
    }

    pub fn align_up<U: Into<u64>>(&self, align: U) -> Result<Self, NonCanonical> {
        let align = align.into();
        assert!(align.is_power_of_two(), "`align` must be a power of two");
        let mask = align - 1;
        if self.raw() & mask == 0 {
            Ok(*self)
        } else if let Some(aligned) = (self.raw() | mask).checked_add(1) {
            Self::new(aligned)
        } else {
            panic!("added with overflow")
        }
    }
}

impl TryFrom<u64> for PhysAddr {
    type Error = NonCanonical;

    fn try_from(addr: u64) -> Result<Self, Self::Error> {
        Self::new(addr)
    }
}

impl TryFrom<usize> for PhysAddr {
    type Error = NonCanonical;

    fn try_from(addr: usize) -> Result<Self, Self::Error> {
        Self::new(addr as u64)
    }
}

impl From<PhysAddr> for u64 {
    fn from(addr: PhysAddr) -> Self {
        addr.0
    }
}

impl From<PhysAddr> for usize {
    fn from(addr: PhysAddr) -> Self {
        addr.0 as usize
    }
}

pub enum Offset {
    Usize(usize),
    Isize(isize),
}

impl From<isize> for Offset {
    fn from(x: isize) -> Self {
        Self::Isize(x)
    }
}

impl From<usize> for Offset {
    fn from(x: usize) -> Self {
        Self::Usize(x)
    }
}

impl Sub<PhysAddr> for PhysAddr {
    type Output = usize;

    fn sub(self, rhs: PhysAddr) -> Self::Output {
        (self.0.checked_sub(rhs.0).unwrap()) as usize
    }
}

impl Sub<VirtAddr> for VirtAddr {
    type Output = usize;

    fn sub(self, rhs: VirtAddr) -> Self::Output {
        (self.0.checked_sub(rhs.0).unwrap()) as usize
    }
}

impl LowerHex for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        LowerHex::fmt(&self.0, f)
    }
}

impl core::fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "V(0x{:x})", self.0)
    }
}

impl core::fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PHYS(0x{:x})", self.0)
    }
}
