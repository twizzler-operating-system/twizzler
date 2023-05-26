use core::ops::Deref;

/// A reference to a memory mapped IO region of
/// memory of type `T`
pub struct MmioRef<T> {
    address: *const T,
}

impl<T> MmioRef<T> {
    pub fn new(address: *const T) -> Self {
        Self {
            address,
        }
    }
}

impl<T> Deref for MmioRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.address }
    }
}
