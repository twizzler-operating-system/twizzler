use super::{ObjectRef, PageNumber};

pub fn copy_ranges(
    src: ObjectRef,
    src_start: PageNumber,
    dest: ObjectRef,
    dest_start: PageNumber,
    length: usize,
) {
    let (src_tree, dest_tree) = crate::utils::lock_two(&src.range_tree, &dest.range_tree);

    let mut i = 0;
    while i < length {
        let range = src_tree.range(src_start..src_start.offset(length));
    }
}
