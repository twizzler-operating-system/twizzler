//! Simple support for volatile memory access.

use core::cell::UnsafeCell;
use core::ptr;

/// A value that should be accessed with volatile memory semantics.
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
    /// Construct a new volatile cell.
    pub const fn new(item: T) -> Self {
        Volatile {
            item: UnsafeCell::new(item),
        }
    }

    /// Volatile-read the cell.
    #[inline(always)]
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        unsafe { ptr::read_volatile(self.item.get()) }
    }

    /// Volatile-write the cell.
    #[inline(always)]
    pub fn set(&self, item: T)
    where
        T: Copy,
    {
        unsafe { ptr::write_volatile(self.item.get(), item) }
    }
}
