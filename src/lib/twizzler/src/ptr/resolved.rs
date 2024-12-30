use std::marker::PhantomData;

use twizzler_rt_abi::object::ObjectHandle;

pub struct Ref<'obj, T> {
    ptr: *const T,
    handle: *const ObjectHandle,
    _pd: PhantomData<&'obj T>,
}
