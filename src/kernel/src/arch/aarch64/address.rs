use core::ops::{Add, AddAssign, Sub};

#[derive(Clone, Copy, Debug)]
pub struct PhysAddr;

impl PhysAddr {
    pub fn new(_address: u64) -> Self {
        todo!()
    }

    pub fn as_u64(self) -> u64 {
        todo!()
    }

    pub fn align_up<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }

    pub fn align_down<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VirtAddr;

impl VirtAddr {
    pub fn new(_address: u64) -> Self {
        todo!()
    }

    pub fn as_u64(self) -> u64 {
        todo!()
    }

    pub fn from_ptr<T>(_ptr: *const T) -> Self {
        todo!()
    }

    pub fn as_ptr<T>(self) -> *const T {
        todo!()
    } 

    pub fn as_mut_ptr<T>(self) -> *mut T {
        todo!()
    }

    pub fn align_up<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }

    pub fn align_down<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }
}

impl Add<usize> for PhysAddr {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        PhysAddr::new(self.as_u64().checked_add(rhs as u64).unwrap())
    }
}

impl Add<u64> for PhysAddr {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        PhysAddr::new(self.as_u64().checked_add(rhs).unwrap())
    }
}

impl Sub<PhysAddr> for PhysAddr {
    type Output = u64;

    fn sub(self, rhs: Self) -> Self::Output {
        self.as_u64().checked_sub(rhs.as_u64()).unwrap()
    }
}

impl core::fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        core::fmt::LowerHex::fmt(&self.as_u64(), f)
    }
}

impl Add<usize> for VirtAddr {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        VirtAddr::new(self.as_u64().checked_add(rhs as u64).unwrap())
    }
}

impl Add<u64> for VirtAddr {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        VirtAddr::new(self.as_u64().checked_add(rhs).unwrap())
    }
}

impl AddAssign<usize> for VirtAddr {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs
    }
}
