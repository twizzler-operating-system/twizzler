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
    let end = match range.end_bound() {
        Bound::Included(n) => n.checked_add(1).unwrap_or(len),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range_bounds_unbounded() {
        let result = range_bounds_to_start_and_end(10, ..);
        assert_eq!(result, (0, 10));
    }

    #[test]
    fn test_range_bounds_start_inclusive() {
        let result = range_bounds_to_start_and_end(10, 3..);
        assert_eq!(result, (3, 10));
    }

    #[test]
    fn test_range_bounds_end_exclusive() {
        let result = range_bounds_to_start_and_end(10, ..7);
        assert_eq!(result, (0, 7));
    }

    #[test]
    fn test_range_bounds_both_inclusive_exclusive() {
        let result = range_bounds_to_start_and_end(10, 2..8);
        assert_eq!(result, (2, 8));
    }

    #[test]
    fn test_range_bounds_both_inclusive() {
        let result = range_bounds_to_start_and_end(10, 2..=7);
        assert_eq!(result, (2, 8));
    }

    #[test]
    fn test_range_bounds_start_exclusive() {
        let result = range_bounds_to_start_and_end(
            10,
            (std::ops::Bound::Excluded(2), std::ops::Bound::Unbounded),
        );
        assert_eq!(result, (3, 10));
    }

    #[test]
    fn test_range_bounds_end_inclusive() {
        let result = range_bounds_to_start_and_end(10, ..=5);
        assert_eq!(result, (0, 6));
    }

    #[test]
    fn test_range_bounds_saturating_add_start() {
        let result = range_bounds_to_start_and_end(
            10,
            (
                std::ops::Bound::Excluded(usize::MAX),
                std::ops::Bound::Unbounded,
            ),
        );
        assert_eq!(result, (usize::MAX, 10));
    }

    #[test]
    fn test_range_bounds_saturating_add_end() {
        let result = range_bounds_to_start_and_end(10, ..=usize::MAX);
        assert_eq!(result, (0, 10));
    }

    #[test]
    fn test_range_bounds_empty_range() {
        let result = range_bounds_to_start_and_end(0, ..);
        assert_eq!(result, (0, 0));
    }

    #[test]
    fn test_range_bounds_zero_start_zero_end() {
        let result = range_bounds_to_start_and_end(10, 0..0);
        assert_eq!(result, (0, 0));
    }

    #[test]
    fn test_range_bounds_same_start_end() {
        let result = range_bounds_to_start_and_end(10, 5..5);
        assert_eq!(result, (5, 5));
    }
}
