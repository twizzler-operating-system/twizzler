use core::cell::UnsafeCell;
use core::ptr;

#[repr(transparent)]
pub struct Volatile<T> {
    item: UnsafeCell<T>,
}

impl<T> core::fmt::Debug for Volatile<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Volatile")
            .field("item", &self.item)
            .finish()
    }
}

impl<T> Volatile<T> {
    pub const fn new(item: T) -> Self {
        Volatile {
            item: UnsafeCell::new(item),
        }
    }

    #[inline(always)]
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        unsafe { ptr::read_volatile(self.item.get()) }
    }

    #[inline(always)]
    pub fn set(&self, item: T)
    where
        T: Copy,
    {
        unsafe { ptr::write_volatile(self.item.get(), item) }
    }
}
