use std::ops::{Bound, RangeBounds};

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
