use core::{convert::From, ops::{Add, AddAssign, Sub}};

use crate::arch::memory::{ArchPhysAddr, ArchVirtAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysAddr {
    inner: ArchPhysAddr
}

impl PhysAddr {
    pub fn new(address: u64) -> Self {
        Self {
            inner: ArchPhysAddr::new(address)
        }
    }

    pub fn as_u64(self) -> u64 {
        self.inner.as_u64()
    }

    pub fn align_up<U>(self, alignment: U) -> Self
    where
        U: Into<u64>
    {
        self.inner.align_up(alignment).into()
    }

    pub fn align_down<U>(self, alignment: U) -> Self
    where
        U: Into<u64>
    {
        self.inner.align_down(alignment).into()
    }
}

impl Add<usize> for PhysAddr {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        PhysAddr::new(self.inner.as_u64().checked_add(rhs as u64).unwrap())
    }
}

impl Add<u64> for PhysAddr {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        PhysAddr::new(self.inner.as_u64().checked_add(rhs).unwrap())
    }
}

impl Sub<PhysAddr> for PhysAddr {
    type Output = u64;

    fn sub(self, rhs: Self) -> Self::Output {
        self.inner.as_u64().checked_sub(rhs.inner.as_u64()).unwrap()
    }
}

impl From<PhysAddr> for ArchPhysAddr {
    fn from(pa: PhysAddr) -> Self {
        pa.inner
    }
}

impl From<ArchPhysAddr> for PhysAddr {
    fn from(pa: ArchPhysAddr) -> Self {
        Self {
            inner: pa
        }
    }
}

impl core::fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        core::fmt::LowerHex::fmt(&self.as_u64(), f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtAddr {
    inner: ArchVirtAddr
}

impl VirtAddr {
    /// Function may panic of address is not canonical depending on the implementation
    pub fn new(address: u64) -> Self {
        Self {
            inner: ArchVirtAddr::new(address)
        }
    }

    pub fn as_u64(self) -> u64 {
        self.inner.as_u64()
    }

    pub fn from_ptr<T>(ptr: *const T) -> Self {
        Self {
            inner: ArchVirtAddr::from_ptr::<T>(ptr)
        }
    }

    pub fn as_ptr<T>(self) -> *const T {
        self.inner.as_ptr::<T>()
    } 

    pub fn as_mut_ptr<T>(self) -> *mut T {
        self.inner.as_mut_ptr::<T>()
    }

    pub fn align_up<U>(self, alignment: U) -> Self
    where
        U: Into<u64>
    {
        self.inner.align_up(alignment).into()
    }

    pub fn align_down<U>(self, alignment: U) -> Self
    where
        U: Into<u64>
    {
        self.inner.align_down(alignment).into()
    }
}

impl Add<usize> for VirtAddr {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        VirtAddr::new(self.inner.as_u64().checked_add(rhs as u64).unwrap())
    }
}

impl Add<u64> for VirtAddr {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        VirtAddr::new(self.inner.as_u64().checked_add(rhs).unwrap())
    }
}

impl AddAssign<usize> for VirtAddr {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs
    }
}

impl From<VirtAddr> for ArchVirtAddr {
    fn from(va: VirtAddr) -> Self {
        va.inner
    }
}

impl From<ArchVirtAddr> for VirtAddr {
    fn from(va: ArchVirtAddr) -> Self {
        Self {
            inner: va
        }
    }
}
