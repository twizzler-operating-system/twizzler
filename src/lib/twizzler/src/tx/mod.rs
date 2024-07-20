use std::{
    cell::UnsafeCell,
    mem::{transmute, MaybeUninit},
    ops::Deref,
};

use crate::marker::{InPlace, Invariant, InvariantValue};

/// A trait for implementing transaction handles.
///
/// Takes a lifetime argument, 'obj. All object handles referenced by this transaction must have
/// this lifetime or longer.
pub trait TxHandle<'obj> {
    /// Ensures transactional safety for mutably accessing data given by the range [data, data +
    /// sizeof(T)).
    fn tx_mut<T, E>(&self, data: *const T) -> TxResult<*mut T, E>;
}

impl<'a, Tx: TxHandle<'a>> TxHandle<'a> for &Tx {
    fn tx_mut<T, E>(&self, data: *const T) -> TxResult<*mut T, E> {
        (*self).tx_mut(data)
    }
}

/// Return type for transactions, containing common errors, Ok value, and user-specified Abort type.
pub type TxResult<T, E = ()> = Result<T, TxError<E>>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Transaction errors, with user-definable abort type.
pub enum TxError<E = ()> {
    /// Transaction aborted.
    Abort(E),
    /// Resources exhausted.
    Exhausted,
    /// Tried to mutate immutable data.
    Immutable,
}

/// A transaction cell, enabling transactional interior mutability.
#[repr(transparent)]
#[derive(Default, Debug, twizzler_derive::Invariant)]
pub struct TxCell<T: Invariant>(UnsafeCell<T>);

unsafe impl<T: Invariant> InvariantValue for TxCell<T> {}

impl<T: Invariant> From<T> for TxCell<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Invariant> TxCell<T> {
    pub fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
    /// Get a mutable reference to the interior data. This function is unsafe because it allows one
    /// to create multiple mutable references simultaneously.
    ///
    /// # Safety
    /// The caller must ensure that no returned reference from this function aliases any other alive
    /// reference to the same TxCell.
    pub unsafe fn as_mut<'a, E>(&self, tx: impl TxHandle<'a>) -> TxResult<&mut T, E> {
        let target = tx.tx_mut(self.0.get())?;
        Ok(&mut *target)
    }

    /// Get a mutable reference to the interior data. Takes a mutable reference to the TxCell to
    /// enforce borrowing rules.
    pub fn get_mut<'a>(&mut self, tx: impl TxHandle<'a>) -> TxResult<&mut T> {
        // Safety: we take self as &mut, so we hold the only reference.
        unsafe { self.as_mut(tx) }
    }

    pub fn modify<'a, R>(&self, f: impl FnOnce(&mut T) -> R, tx: impl TxHandle<'a>) -> TxResult<R> {
        unsafe {
            let ptr = self.as_mut(tx)?;
            Ok(f(ptr))
        }
    }
}

impl<'a, T: Invariant> TxCell<T> {
    /// Set the value of the cell, constructing the value in-place.
    pub fn set_with<F>(&self, ctor: F, tx: impl TxHandle<'a>) -> TxResult<()>
    where
        F: FnOnce(&mut InPlace<'_>) -> T,
    {
        let ptr = unsafe { transmute::<&mut T, &mut MaybeUninit<T>>(self.as_mut(&tx)?) };
        let handle = twizzler_runtime_api::get_runtime()
            .ptr_to_handle(ptr.as_mut_ptr() as *const u8)
            .unwrap()
            .0; // TODO: unwrap
        let mut in_place = InPlace::new(&handle);
        let value = ctor(&mut in_place);
        let ptr = unsafe { transmute::<&mut T, &mut MaybeUninit<T>>(self.as_mut(&tx)?) };
        ptr.write(value);
        Ok(())
    }
}

impl<'a, T: Invariant + 'a> TxCell<T> {
    /// Set the value of the cell, constructing the value in-place.
    pub fn set(&self, value: T, tx: impl TxHandle<'a>) -> TxResult<()> {
        unsafe {
            let ptr = self.as_mut(tx)? as *mut T;
            ptr.write(value);
        }
        Ok(())
    }
}

impl<T: Invariant> Deref for TxCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.get() }
    }
}

#[derive(Clone, Copy)]
pub struct UnsafeTxHandle {
    _priv: (),
}

impl<'a> TxHandle<'a> for UnsafeTxHandle {
    fn tx_mut<T, E>(&self, data: *const T) -> crate::tx::TxResult<*mut T, E> {
        Ok(data as *mut T)
    }
}

impl UnsafeTxHandle {
    pub const unsafe fn new() -> Self {
        Self { _priv: () }
    }
}
