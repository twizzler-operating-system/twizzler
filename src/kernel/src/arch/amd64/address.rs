use core::{fmt::LowerHex, ops::Sub};

use super::memory::phys_to_virt;

#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
pub struct VirtAddr(u64);

#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
pub struct PhysAddr(u64);

#[derive(Debug, Clone, Copy)]
pub struct NonCanonical;

impl VirtAddr {
    pub const fn start_kernel_memory() -> Self {
        Self(0xffff800000000000)
    }

    pub const fn new(addr: u64) -> Result<Self, NonCanonical> {
        if addr >= 0xFFFF800000000000 || addr <= 0x00007fffffffffff {
            Ok(Self(addr))
        } else {
            Err(NonCanonical)
        }
    }

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
        self.0 >= 0xffff800000000000
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
        } else {
            if let Some(aligned) = (self.raw() | mask).checked_add(1) {
                Self::new(aligned)
            } else {
                Err(NonCanonical)
            }
        }
    }
}

impl<T> From<&mut T> for VirtAddr {
    fn from(x: &mut T) -> Self {
        Self((x as *mut T) as usize as u64)
    }
}

impl<T> From<&T> for VirtAddr {
    fn from(x: &T) -> Self {
        Self((x as *const T) as usize as u64)
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

impl PhysAddr {
    pub fn new(addr: u64) -> Result<Self, NonCanonical> {
        //TODO: Check if the address is canonical
        Ok(Self(addr))
    }

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
        } else {
            if let Some(aligned) = (self.raw() | mask).checked_add(1) {
                Self::new(aligned)
            } else {
                panic!("added with overflow")
            }
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
