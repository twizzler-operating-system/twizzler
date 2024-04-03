use twizzler_runtime_api::ObjectHandle;

use crate::mapman::MapHandle;

pub(super) struct CompThread {
    thread_repr: ObjectHandle,
    tls_object: MapHandle,
}
