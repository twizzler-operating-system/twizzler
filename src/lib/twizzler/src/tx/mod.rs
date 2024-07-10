use std::ops::{Deref, DerefMut};

/// A trait for implementing transaction handles.
///
/// Takes a lifetime argument, 'obj. All object handles referenced by this transaction must have
/// this lifetime or longer.
pub trait TxHandle<'obj> {
    /// Ensures transactional safety for accessing
    fn tx_cell_mut<T, E>(&self, data: *const T) -> TxResult<(), E>;
}

impl<'a, Tx: TxHandle<'a>> TxHandle<'a> for &Tx {
    fn tx_cell_mut<T, E>(&self, data: *const T) -> TxResult<(), E> {
        todo!()
    }
}

pub type TxResult<T, E = ()> = Result<T, TxError<E>>;

#[derive(Debug)]
pub enum TxError<E> {
    Abort(E),
    Exhausted,
    Immutable,
}

#[repr(transparent)]
pub struct TxObjectCell<T>(T);

impl<T> TxObjectCell<T> {
    pub fn as_mut<'a, E>(&'a self, tx: impl TxHandle<'a>) -> TxResult<&mut Self, E> {
        tx.tx_cell_mut(&self.0)?;
        // Safety: The TxHandle ensures safety here.
        unsafe {
            Ok(((self) as *const _ as *mut Self)
                .as_mut()
                .unwrap_unchecked())
        }
    }
}

impl<T> Deref for TxObjectCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// TODO: is this safe to do? We need a &mut self, which would be non-trivial to get...
impl<T> DerefMut for TxObjectCell<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

mod test {
    use super::{TxHandle, TxObjectCell};

    fn test<'a>(tc: &'a TxObjectCell<u32>, th: impl TxHandle<'a>) {
        // TODO: this should not compile!
        let p1 = tc.as_mut::<'a, ()>(&th).unwrap();
        let p2 = tc.as_mut::<'a, ()>(&th).unwrap();
        **p1 = 2;
        **p2 = 3;
    }
}
