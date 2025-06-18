use std::ops::{Bound, RangeBounds};

use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

use crate::object::RawObject;

pub(crate) fn range_bounds_to_start_and_end(
    len: usize,
    range: impl RangeBounds<usize>,
) -> (usize, usize) {
    let start = match range.start_bound() {
        Bound::Included(n) => *n,
        Bound::Excluded(n) => n.saturating_add(1),
        Bound::Unbounded => 0,
    };
    let end = match range.start_bound() {
        Bound::Included(n) => n.saturating_add(1),
        Bound::Excluded(n) => *n,
        Bound::Unbounded => len,
    };
    (start, end)
}

pub(crate) fn maybe_remap<T>(handle: ObjectHandle, ptr: *mut T) -> (ObjectHandle, *mut T) {
    if !handle.map_flags().contains(MapFlags::WRITE) {
        let new_handle = twizzler_rt_abi::object::twz_rt_map_object(
            handle.id(),
            MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST,
        )
        .expect("failed to remap object handle for writing");
        let ptr = if ptr.is_null() {
            ptr
        } else {
            let offset = handle
                .ptr_local(ptr.cast())
                .expect("tried to remap a handle with a non-local pointer");
            new_handle
                .lea_mut(offset, size_of::<T>())
                .expect("failed to remap pointer")
                .cast()
        };
        (new_handle, ptr)
    } else {
        (handle, ptr)
    }
}
