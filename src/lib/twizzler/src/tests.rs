use std::mem::size_of;

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_runtime_api::ObjectHandle;

use crate::{
    object::{BaseType, Object, RawObject},
    tx::{TxHandle, TxResult},
};

pub(crate) struct TestTxHandle {
    obj_r: ObjectHandle,
    obj_w: ObjectHandle,
}

pub fn object_tx<B: BaseType, R>(
    obj: &Object<B>,
    tx: impl FnOnce(TestTxHandle) -> TxResult<R>,
) -> TxResult<R> {
    let h = TestTxHandle {
        obj_r: obj.handle().clone(),
        obj_w: obj.handle().clone(),
    };
    tx(h)
}

fn is_in_object<T>(ptr: *const T, obj: &ObjectHandle) -> bool {
    ptr.addr() >= obj.base_ptr().addr()
        && ptr.addr() < obj.base_ptr().addr() + MAX_SIZE - NULLPAGE_SIZE
}

impl<'a> TxHandle<'a> for TestTxHandle {
    fn tx_mut<T, E>(&self, data: *const T) -> crate::tx::TxResult<*mut T, E> {
        if is_in_object(data, &self.obj_r) {
            Ok(self
                .obj_w
                .lea_mut(
                    self.obj_r.ptr_local(data as *const u8).unwrap(),
                    size_of::<T>(),
                )
                .unwrap() as *mut T)
        } else if is_in_object(data, &self.obj_w) {
            Ok(self
                .obj_w
                .lea_mut(
                    self.obj_w.ptr_local(data as *const u8).unwrap(),
                    size_of::<T>(),
                )
                .unwrap() as *mut T)
        } else {
            panic!("invalid pointer passed to TestTxHandle");
        }
    }
}
