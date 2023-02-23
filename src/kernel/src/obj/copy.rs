use crate::mutex::LockGuard;

use super::{
    range::{PageRange, PageRangeTree},
    InvalidateMode, ObjectRef, PageNumber,
};

fn split_range(
    range: PageRange,
    out: core::ops::Range<PageNumber>,
) -> (Option<PageRange>, Option<PageRange>) {
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
    dest_tree: &mut LockGuard<PageRangeTree>,
    dest_point: PageNumber,
    range: &PageRange,
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
            r1.gc_pagevec();
            let res = dest_tree.insert_replace(r1.start..r1.start.offset(r1.length), r1);
            assert!(res.is_empty());
        }
        if let Some(r2) = r2 {
            r2.gc_pagevec();
            let res = dest_tree.insert_replace(r2.start..r2.start.offset(r2.length), r2);
            assert!(res.is_empty());
        }
    }
}

pub fn copy_ranges(
    src: &ObjectRef,
    src_start: PageNumber,
    dest: &ObjectRef,
    dest_start: PageNumber,
    length: usize,
) {
    let (src_tree, mut dest_tree) = crate::utils::lock_two(&src.range_tree, &dest.range_tree);

    let mut dest_point = dest_start;
    let mut src_point = src_start;
    let mut rem = length;
    let ranges = src_tree.range(src_start..src_start.offset(length));
    for range in ranges {
        if src_point < *range.0 {
            /* TODO: we'll need to ensure all backing pages are present if we get here */
            let diff = *range.0 - src_point;
            dest_point = dest_point.offset(diff);
            rem -= diff;
        }
        let offset = src_point.num().saturating_sub(range.0.num());
        let len = core::cmp::min(range.1.value().length - offset, rem);
        copy_range_to_object_tree(&mut dest_tree, dest_point, range.1.value(), offset, len);
        dest_point = dest_point.offset(len);
        rem -= len;
        src_point = src_point.offset(len);
    }

    src.invalidate(
        src_start..src_start.offset(length),
        InvalidateMode::WriteProtect,
    );
    dest.invalidate(dest_start..dest_start.offset(length), InvalidateMode::Full);
}

pub struct CopySpec {
    pub src: ObjectRef,
    pub src_start: PageNumber,
    pub dest_start: PageNumber,
    pub length: usize,
}

impl CopySpec {
    pub fn new(
        src: ObjectRef,
        src_start: PageNumber,
        dest_start: PageNumber,
        length: usize,
    ) -> Self {
        Self {
            src,
            src_start,
            dest_start,
            length,
        }
    }
}
