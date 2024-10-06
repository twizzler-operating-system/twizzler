use std::{
    cell::{RefCell, UnsafeCell},
    mem::{size_of, transmute, MaybeUninit},
    ops::{Deref, RangeInclusive},
    pin::Pin,
};

use range_set::RangeSet;
use twizzler_runtime_api::{get_runtime, ObjectHandle};

use crate::{
    marker::{Invariant, InvariantValue, StorePlace},
    object::{BaseType, RawObject},
};

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
    /// Invalid argument.
    InvalidArgument,
}

impl<E> From<E> for TxError<E> {
    fn from(value: E) -> Self {
        Self::Abort(value)
    }
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
    pub unsafe fn as_mut<'a, E>(&self, tx: impl TxHandle<'a>) -> TxResult<Pin<&mut T>, E> {
        let target = tx.tx_mut(self.0.get())?;
        let ptr = Pin::new_unchecked(&mut *target);
        Ok(ptr)
    }

    /// Get a mutable reference to the interior data. Takes a mutable reference to the TxCell to
    /// enforce borrowing rules.
    pub fn get_mut<'a>(&mut self, tx: impl TxHandle<'a>) -> TxResult<Pin<&mut T>> {
        // Safety: we take self as &mut, so we hold the only reference.
        unsafe { self.as_mut(tx) }
    }

    pub fn modify<'a, R, E>(
        &self,
        f: impl FnOnce(Pin<&mut T>) -> R,
        tx: impl TxHandle<'a>,
    ) -> TxResult<R, E> {
        unsafe {
            let ptr = self.as_mut(tx)?;
            Ok(f(ptr))
        }
    }

    pub fn try_modify<'a, R, E>(
        &self,
        f: impl FnOnce(Pin<&mut T>) -> TxResult<R, E>,
        tx: impl TxHandle<'a>,
    ) -> TxResult<R, E> {
        unsafe {
            let ptr = self.as_mut(tx)?;
            f(ptr)
        }
    }
}

impl<'a, T: Invariant> TxCell<T> {
    /// Set the value of the cell, constructing the value in-place.
    pub fn set_with<F>(&self, ctor: F, tx: impl TxHandle<'a>) -> TxResult<()>
    where
        F: FnOnce(&mut StorePlace<'_>) -> T,
    {
        // TODO: do we need to drop anything?
        let ptr = unsafe { transmute::<Pin<&mut T>, Pin<&mut MaybeUninit<T>>>(self.as_mut(&tx)?) };
        let handle = twizzler_runtime_api::get_runtime()
            .ptr_to_handle(ptr.as_ptr() as *const u8)
            .unwrap()
            .0; // TODO: unwrap
        let mut in_place = StorePlace::new(&handle);
        let value = ctor(&mut in_place);
        let ptr = unsafe { transmute::<Pin<&mut T>, Pin<&mut MaybeUninit<T>>>(self.as_mut(&tx)?) };
        // TODO: is this okay?
        unsafe { ptr.get_unchecked_mut().write(value) };
        Ok(())
    }
}

impl<'a, T: Invariant + 'a + Unpin> TxCell<T> {
    /// Set the value of the cell, constructing the value in-place.
    pub fn set(&self, value: T, tx: impl TxHandle<'a>) -> TxResult<()> {
        // TODO: do we need to drop anything?
        unsafe {
            let ptr = self.as_mut(tx)?.get_mut() as *mut T;
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

const CHANGE_SET_STACK_SIZE: usize = 8;
pub struct ObjectTxHandle<'a> {
    handle: &'a ObjectHandle,
    change_set: RefCell<RangeSet<[RangeInclusive<u64>; CHANGE_SET_STACK_SIZE]>>,
}

impl<'a> ObjectTxHandle<'a> {
    pub fn new(handle: &'a ObjectHandle) -> Self {
        Self {
            handle,
            change_set: RefCell::new(RangeSet::new()),
        }
    }
}

impl<'a> TxHandle<'a> for ObjectTxHandle<'a> {
    fn tx_mut<T, E>(&self, data: *const T) -> TxResult<*mut T, E> {
        // TODO: check if pointer is in this object
        // TODO: ensure uniqueness of returned pointers?
        let len = size_of::<T>();
        if len == 0 {
            return Ok(data as *mut T);
        }
        let runtime = get_runtime();
        let (_, offset) = runtime
            .ptr_to_object_start(data.cast(), len)
            .ok_or(TxError::InvalidArgument)?;
        let off = offset as u64;
        let _existing = self
            .change_set
            .borrow_mut()
            .insert_range(off..=(off + len as u64 - 1));
        Ok(data as *mut T)
    }
}

impl<T: BaseType> crate::object::Object<T> {
    pub fn tx<F, R, E>(&self, f: F) -> TxResult<R, E>
    where
        F: FnOnce(&ObjectTxHandle) -> TxResult<R, E>,
    {
        // 1. Prove uniqueness.
        // TODO

        // 2. Do transaction.
        let tx = ObjectTxHandle::new(self.handle());
        let res = f(&tx);

        if res.is_ok() {
            // 3a. Commit.
            // TODO: call pager, commit
        } else {
            // 3b. Abort.
            // TODO: call pager, abort
        }

        res
    }
}