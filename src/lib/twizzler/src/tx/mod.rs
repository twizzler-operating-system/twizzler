use std::{
    cell::UnsafeCell,
    mem::{transmute, MaybeUninit},
    ops::Deref,
};

use crate::marker::InPlaceCtor;

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

#[derive(Debug)]
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
#[derive(Default, Debug)]
pub struct TxCell<T>(UnsafeCell<T>);

impl<T> From<T> for TxCell<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> TxCell<T> {
    pub fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
    /// Get a mutable reference to the interior data. This function is unsafe because it allows one
    /// to create multiple mutable references simultaneously.
    ///
    /// # Safety
    /// The caller must ensure that no returned reference from this function aliases any other alive
    /// reference to the same TxCell.
    pub unsafe fn as_mut<'a>(&self, tx: impl TxHandle<'a>) -> TxResult<&mut T> {
        let target = tx.tx_mut(self.0.get())?;
        Ok(&mut *target)
    }

    /// Get a mutable reference to the interior data. Takes a mutable reference to the TxCell to
    /// enforce borrowing rules.
    pub fn get_mut<'a>(&mut self, tx: impl TxHandle<'a>) -> TxResult<&mut T> {
        // Safety: we take self as &mut, so we hold the only reference.
        unsafe { self.as_mut(tx) }
    }
}

impl<'a, T: InPlaceCtor + 'a> TxCell<T> {
    /// Set the value of the cell, constructing the value in-place.
    pub fn set_in_place(&self, value: T::Builder, tx: impl TxHandle<'a>) -> TxResult<()> {
        let ptr = unsafe { transmute::<&mut T, &mut MaybeUninit<T>>(self.as_mut(&tx)?) };
        T::in_place_ctor(value, ptr, &tx);
        Ok(())
    }
}

impl<'a, T: Copy> TxCell<T> {
    /// Set the value of the cell, constructing the value in-place.
    pub fn set(&self, value: T, tx: impl TxHandle<'a>) -> TxResult<()> {
        self.set_in_place(value, tx)
    }
}

unsafe impl<T: InPlaceCtor> InPlaceCtor for TxCell<T> {
    type Builder = T::Builder;

    fn in_place_ctor<'b>(
        builder: Self::Builder,
        place: &'b mut MaybeUninit<Self>,
        tx: impl TxHandle<'b>,
    ) -> &'b mut Self
    where
        Self: Sized,
    {
        unsafe {
            let inner_place = transmute::<&mut MaybeUninit<Self>, &mut MaybeUninit<T>>(place);
            T::in_place_ctor(builder, inner_place, tx);
            place.assume_init_mut()
        }
    }
}

impl<T> Deref for TxCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.get() }
    }
}

/*
mod test {
    use super::{TxCell, TxHandle};

    fn test<'a>(tc: &'a TxCell<u32>, mut th: impl TxHandle<'a>) {
        // TODO: this should not compile!
        let p1 = tc.as_mut::<'a, ()>(&th).unwrap();
        let p2 = tc.as_mut::<'a, ()>(&th).unwrap();
        *p1 = 2;
        *p2 = 3;
    }

    fn test<'a>(tc: &'a TxCell<u32>, mut th: impl TxHandle<'a>) {
        let p1 = tc.as_mut::<'a, ()>(&th).unwrap();
        *p1 = 2;
    }
}
*/
