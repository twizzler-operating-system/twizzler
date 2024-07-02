use twizzler_runtime_api::ObjectHandle;

pub struct TxHandle<'obj> {
    object: &'obj ObjectHandle,
}

pub type TxResult<T, E> = Result<T, TxError<E>>;

pub enum TxError<E> {
    Abort(E),
    Exhausted,
    Immutable,
}

#[repr(transparent)]
pub struct TxObjectCell<T>(T);

impl<T> TxObjectCell<T> {
    pub fn as_ref(&self, tx: &TxHandle<'_>) -> &T {
        todo!()
    }

    pub fn as_mut(&self, tx: &mut TxHandle<'_>) -> &mut T {
        todo!()
    }

    pub fn write(&self, tx: &mut TxHandle<'_>, data: T) {
        todo!()
    }
}
