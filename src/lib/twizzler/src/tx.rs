mod batch;
mod object;
mod reference;
mod unsafetx;

use std::{alloc::AllocError, cell::UnsafeCell, mem::MaybeUninit};

pub use batch::*;
pub use object::*;
pub use reference::*;
use twizzler_rt_abi::object::MapError;
pub use unsafetx::*;

use crate::{
    alloc::{invbox::InvBox, Allocator},
    marker::Invariant,
    object::CreateError,
};

/// A trait for implementing per-object transaction handles.
pub trait TxHandle {
    /// Ensures transactional safety for mutably accessing data in the range [data, data + len).
    fn tx_mut(&self, data: *const u8, len: usize) -> Result<*mut u8>;

    fn write_uninit<T>(&self, target: &mut MaybeUninit<T>, value: T) -> Result<&mut T> {
        let ptr = self
            .tx_mut((target as *mut MaybeUninit<T>).cast(), size_of::<T>())?
            .cast::<MaybeUninit<T>>();
        let target = unsafe { &mut *ptr };
        Ok(target.write(value))
    }

    fn new_box_with<T: Invariant, A: Allocator, F>(
        &self,
        _alloc: &A,
        _ctor: F,
    ) -> Result<InvBox<T, A>>
    where
        F: FnOnce(&mut MaybeUninit<T>),
    {
        todo!()
    }
}

pub type Result<T> = std::result::Result<T, TxError>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
/// Transaction errors, with user-definable abort type.
pub enum TxError {
    /// Resources exhausted.
    #[error("resources exhausted")]
    Exhausted,
    /// Tried to mutate immutable data.
    #[error("object is immutable")]
    Immutable,
    /// Invalid argument.
    #[error("invalid argument")]
    InvalidArgument,
    /// Create error
    #[error("create error")]
    CreateError(#[from] CreateError),
    /// Map error
    #[error("mapping error")]
    MapError(#[from] MapError),
}

#[repr(transparent)]
pub struct TxCell<T>(UnsafeCell<T>);

impl<T: Clone> Clone for TxCell<T> {
    fn clone(&self) -> Self {
        Self(UnsafeCell::new(unsafe {
            self.0.get().as_ref().unwrap().clone()
        }))
    }
}

impl<T> TxCell<T> {
    pub fn new(inner: T) -> Self {
        Self(UnsafeCell::new(inner))
    }

    pub unsafe fn as_mut(&self) -> &mut T {
        unsafe { self.0.get().as_mut().unwrap_unchecked() }
    }

    pub fn get_mut(&self, tx: &impl TxHandle) -> Result<&mut T> {
        let inner = self.0.get();
        let ptr = tx.tx_mut(inner.cast(), size_of::<T>())?;
        unsafe { Ok(ptr.cast::<T>().as_mut().unwrap_unchecked()) }
    }
}

impl<T> std::ops::Deref for TxCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.get().as_ref().unwrap() }
    }
}

impl From<AllocError> for TxError {
    fn from(_value: AllocError) -> Self {
        TxError::Exhausted
    }
}
