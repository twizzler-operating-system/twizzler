mod batch;
mod object;
mod reference;
mod unsafetx;

use std::{alloc::AllocError, mem::MaybeUninit};

pub use batch::*;
pub use object::*;
pub use reference::*;
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
        todo!()
    }

    fn ctor_inplace<T, F>(&self, target: &MaybeUninit<T>, ctor: F) -> Result<()>
    where
        F: FnOnce(&mut MaybeUninit<T>) -> Result<()>,
    {
        todo!()
    }

    fn new_box_with<T: Invariant, A: Allocator, F>(
        &self,
        alloc: &A,
        ctor: F,
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
}

#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(transparent)]
pub struct TxCell<T>(T);

impl<T> TxCell<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    pub unsafe fn as_mut(&self) -> &mut T {
        todo!()
    }

    pub fn get_mut(&self, tx: &impl TxHandle) -> Result<&mut T> {
        todo!()
    }
}

impl<T> std::ops::Deref for TxCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<AllocError> for TxError {
    fn from(value: AllocError) -> Self {
        todo!()
    }
}
