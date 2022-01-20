use crate::mutex::LockGuard;

use super::{
    range::{Range, RangeTree},
    ObjectRef, PageNumber,
};

fn split_range(range: Range, out: core::ops::Range<PageNumber>) -> (Option<Range>, Option<Range>) {
    let r1 = if range.start < out.start {
        Some(range.new_from(range.start, range.offset, out.start - range.start))
    } else {
        None
    };
    let end = range.start.offset(range.length);
    let r2 = if end > out.end {
        let diff = out.end - range.start;
        Some(range.new_from(out.end, range.offset + diff, end - out.end))
    } else {
        None
    };
    (r1, r2)
}

fn copy_range_to_object_tree(
    dest_tree: &mut LockGuard<RangeTree>,
    dest_point: PageNumber,
    range: &Range,
    offset: usize,
    length: usize,
) {
    let new_offset = range.offset + offset;
    let new_range = range.new_from(dest_point, new_offset, length);
    let new_range_key = new_range.start..new_range.start.offset(new_range.length);
    let kicked = dest_tree.insert_replace(new_range_key.clone(), new_range);
    for k in kicked {
        let (r1, r2) = split_range(k.1, new_range_key.clone());
        if let Some(r1) = r1 {
            let res = dest_tree.insert_replace(r1.start..r1.start.offset(r1.length), r1);
            assert!(res.len() == 0);
        }
        if let Some(r2) = r2 {
            let res = dest_tree.insert_replace(r2.start..r2.start.offset(r2.length), r2);
            assert!(res.len() == 0);
        }
    }
}

pub fn copy_ranges(
    src: ObjectRef,
    src_start: PageNumber,
    dest: ObjectRef,
    dest_start: PageNumber,
    length: usize,
) {
    let (src_tree, mut dest_tree) = crate::utils::lock_two(&src.range_tree, &dest.range_tree);

    let mut dest_point = dest_start;
    let mut rem = length;
    let ranges = src_tree.range(src_start..src_start.offset(length));
    for range in ranges {
        assert!(src_start >= *range.0);
        let offset = src_start - *range.0;
        let len = core::cmp::min(range.1.value().length - offset, rem);
        copy_range_to_object_tree(&mut dest_tree, dest_point, range.1.value(), offset, len);
        dest_point = dest_point.offset(len);
        rem -= len;
    }
}
