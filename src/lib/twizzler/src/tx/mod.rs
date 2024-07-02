pub struct TxHandle {}

type TxResult<T, E> = Result<T, TxError<E>>;

pub enum TxError<E> {
    Abort(E),
    Exhausted,
    Immutable,
}

#[repr(transparent)]
pub struct TxObjectCell<T>(T);

impl<T> TxObjectCell<T> {
    pub fn tx<TxFn, Ret, Err>(&self, tx: TxFn) -> TxResult<Ret, Err>
    where
        TxFn: FnOnce(&mut T) -> TxResult<Ret, Err>,
    {
        todo!()
    }
}
