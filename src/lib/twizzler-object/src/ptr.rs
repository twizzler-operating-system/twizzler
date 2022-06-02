use std::{
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::Object;

/// The raw invariant pointer, containing just a 64-bit packed FOT entry and offset.
#[repr(transparent)]
pub struct InvPtr<T> {
    raw: u64,
    _pd: PhantomData<T>,
}

impl<T> !Unpin for InvPtr<T> {}

impl<T> Object<T> {
    /// Get a raw pointer into an object given an offset.
    #[inline]
    pub fn raw_lea<P>(&self, off: usize) -> *const P {
        self.slot.raw_lea(off)
    }

    /// Get a raw mutable pointer into an object given an offset.
    #[inline]
    pub fn raw_lea_mut<P>(&self, off: usize) -> *mut P {
        self.slot.raw_lea_mut(off)
    }
}

fn ipoffset(raw: u64) -> u64 {
    raw & 0x0000ffffffffffff
}

fn ipfote(raw: u64) -> u64 {
    (raw & !0x0000ffffffffffff) >> 48
}

impl<Target> InvPtr<Target> {
    /// Read the invariant pointer into its raw parts.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)).
    pub unsafe fn parts_unguarded(&self) -> (usize, u64) {
        let raw = self.raw;
        (ipfote(raw) as usize, ipoffset(raw))
    }

    /// Read the invariant pointer into its raw parts.
    pub fn parts(&mut self) -> (usize, u64) {
        let raw = self.raw;
        (ipfote(raw) as usize, ipoffset(raw))
    }

    /// Construct an InvPtr from an FOT entry and an offset.
    pub fn from_parts(fote: usize, off: u64) -> Self {
        Self {
            raw: (fote << 48) as u64 | (off & 0x0000ffffffffffff),
            _pd: PhantomData,
        }
    }

    /// Check if an invariant pointer is null.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)).
    pub unsafe fn is_null_unguarded(&self) -> bool {
        self.raw == 0
    }

    /// Check if an invariant pointer is null.
    pub fn is_null(&mut self) -> bool {
        self.raw == 0
    }

    /// Construct a null raw pointer.
    pub fn null() -> Self {
        Self {
            raw: 0,
            _pd: PhantomData,
        }
    }

    /// Get a reference to the inner raw 64 bits of the invariant pointer.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)). Additionally, the caller is
    /// expected to maintain the correct semantics of invariant pointers.
    pub unsafe fn raw_inner(&mut self) -> *mut u64 {
        &mut self.raw as *mut u64
    }
}

/// An atomic invariant pointer. Allows reading through an immutable reference without unsafe.
#[repr(transparent)]
pub struct AtomicInvPtr<T> {
    raw: AtomicU64,
    _pd: PhantomData<T>,
}

impl<T> !Unpin for AtomicInvPtr<T> {}

impl<Target> AtomicInvPtr<Target> {
    /// Read the invariant pointer into its raw parts.
    pub fn parts(&self) -> (usize, u64) {
        let raw = self.raw.load(Ordering::SeqCst);
        (ipfote(raw) as usize, ipoffset(raw))
    }

    /// Construct an InvPtr from an FOT entry and an offset.
    pub fn from_parts(fote: usize, off: u64) -> Self {
        Self {
            raw: AtomicU64::new((fote << 48) as u64 | (off & 0x0000ffffffffffff)),
            _pd: PhantomData,
        }
    }

    /// Check if an invariant pointer is null.
    pub fn is_null(&self) -> bool {
        let raw = self.raw.load(Ordering::SeqCst);
        raw == 0
    }

    /// Construct a null raw pointer.
    pub fn null() -> Self {
        Self {
            raw: AtomicU64::new(0),
            _pd: PhantomData,
        }
    }

    /// Get a reference to the inner raw 64 bits of the invariant pointer.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)). Additionally, the caller is
    /// expected to maintain the correct semantics of invariant pointers.
    pub unsafe fn inner(&mut self) -> *mut AtomicU64 {
        &mut self.raw as *mut AtomicU64
    }
}
