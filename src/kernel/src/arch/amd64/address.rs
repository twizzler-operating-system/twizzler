use super::memory::phys_to_virt;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
pub struct VirtAddr(u64);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
pub struct PhysAddr(u64);

#[derive(Debug, Clone, Copy)]
pub enum NonCanonical {}

impl VirtAddr {
    pub const fn new(addr: u64) -> Result<Self, NonCanonical> {
        // TODO: Check if the address is canonical
        Ok(Self(addr))
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

    pub fn offset(&self, offset: isize) -> Result<Self, NonCanonical> {
        Self::new(self.0.wrapping_add(offset as u64))
    }

    pub fn raw(&self) -> u64 {
        self.0
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
        // TODO
        phys_to_virt(x86_64::PhysAddr::new(self.0))
            .as_u64()
            .try_into()
            .unwrap()
    }

    pub fn offset(&self, offset: isize) -> Result<Self, NonCanonical> {
        Self::new(self.0.wrapping_add(offset as u64))
    }

    pub fn is_aligned_to(&self, alignment: usize) -> bool {
        self.0 % alignment as u64 == 0
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
