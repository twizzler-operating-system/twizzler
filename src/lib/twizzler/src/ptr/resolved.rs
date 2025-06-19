use std::{borrow::Cow, cell::OnceCell};

use twizzler_rt_abi::object::ObjectHandle;

mod resolved;
mod resolved_mut;
mod resolved_slice;
mod resolved_slice_mut;
mod resolved_tx;
mod resolved_tx_slice;

pub use resolved::*;
pub use resolved_mut::*;
pub use resolved_slice::*;
pub use resolved_slice_mut::*;
pub use resolved_tx::*;
pub use resolved_tx_slice::*;

#[derive(Default, Clone)]
struct LazyHandle<'obj> {
    handle: OnceCell<Cow<'obj, ObjectHandle>>,
}

impl<'obj> LazyHandle<'obj> {
    fn handle(&self, ptr: *const u8) -> &ObjectHandle {
        self.handle.get_or_init(|| {
            let handle = twizzler_rt_abi::object::twz_rt_get_object_handle(ptr).unwrap();
            Cow::Owned(handle)
        })
    }

    fn new_owned(handle: ObjectHandle) -> Self {
        Self {
            handle: OnceCell::from(Cow::Owned(handle)),
        }
    }

    fn new_borrowed(handle: &'obj ObjectHandle) -> Self {
        Self {
            handle: OnceCell::from(Cow::Borrowed(handle)),
        }
    }
}
