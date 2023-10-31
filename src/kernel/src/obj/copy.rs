use crate::mutex::LockGuard;

use super::{
    range::{PageRange, PageRangeTree},
    InvalidateMode, ObjectRef, PageNumber,
};

// Given a page range and a subrange within it, split it into two parts, the part before the subrange, and the part after.
// Each part may be None if its length is zero (consider splitting [1,2,3,4] with the subrange [1,2] => (None, Some([3,4]))).
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

// Add a page range to the object page tree. We are given: (1) a range we want to take from, (2) a subrange within that range (specified by offset and length),
// and a point to insert this into (dest_point).
fn copy_range_to_object_tree(
    dest_tree: &mut LockGuard<PageRangeTree>,
    dest_point: PageNumber,
    range: &PageRange,
    offset: usize,
    length: usize,
) {
    // First, make a new range that represents the subrange range[offset..(offset + length)].
    let new_offset = range.offset + offset;
    let new_range = range.new_from(dest_point, new_offset, length);
    let new_range_key = new_range.start..new_range.start.offset(new_range.length);
    // Now insert the new range. This will, of course, kick any ranges that overlap with the new range out of the tree, so we
    // need to split those and add in pages that shouldn't have been replaced.
    let kicked = dest_tree.insert_replace(new_range_key.clone(), new_range);
    for k in kicked {
        logln!("kicked: {:?}", k.0);
        // We need to split any kicked ranges into parts that don't overlap with new_range_key, and then reinsert those splits.
        let (r1, r2) = split_range(k.1, new_range_key.clone());
        if let Some(mut r1) = r1 {
            logln!("reins: {:?} {}", r1.range(), r1.start);
            r1.gc_pagevec();
            let res = dest_tree.insert_replace(r1.start..r1.start.offset(r1.length), r1);
            assert!(res.is_empty());
        }
        if let Some(mut r2) = r2 {
            logln!("reins: {:?} {}", r2.range(), r2.start);
            r2.gc_pagevec();
            let res = dest_tree.insert_replace(r2.start..r2.start.offset(r2.length), r2);
            assert!(res.is_empty());
        }
    }
}

// Copy a single, partial page.
fn copy_single(
    dest_tree: &mut LockGuard<PageRangeTree>,
    src_tree: &mut LockGuard<PageRangeTree>,
    dest_point: PageNumber,
    src_point: PageNumber,
    offset: usize,
    max: usize,
) {
    let src_page = src_tree.get_page(src_point, false);
    if dest_tree.get_page(dest_point, true).is_none() {
        // TODO
        dest_tree.add_page(dest_point, super::pages::Page::new());
    }
    let (dest_page, _) = dest_tree
        .get_page(dest_point, true)
        .expect("failed to get destination page"); //TODO fix this
    if let Some((src_page, _)) = src_page {
        dest_page.as_mut_slice()[offset..max].copy_from_slice(&src_page.as_slice()[offset..max]);
    } else {
        // TODO: could skip this on freshly created page, if we can detect that
        dest_page.as_mut_slice()[offset..max].fill(0);
    }
}

/// Copy page ranges from one object to another, preferring to share page vectors if possible.
/// We allow non-page-aligned offsets, as long as the dest and src offsets are mis-aligned by the
/// same amount. In the case that a full page needs to be copied, it will likely be shared and set
/// to copy on write. In the case that a page needs to be partially copied, we'll do a manual copy
/// for that page. This only happens at the start and end of the copy region.
///
/// We lock the page trees for each object (in a canonical order) and ensure that the regions are
/// remapped appropriately for any mapping of the objects. This ensures that the source object is
/// "checkpointed" before copying, and that the destination object cannot be read in the region being
/// overwritten until the copy is done.
pub fn copy_ranges(
    src: &ObjectRef,
    src_off: usize,
    dest: &ObjectRef,
    dest_off: usize,
    byte_length: usize,
) {
    // TODO: support full manual copy, if it comes to that.
    if src_off % PageNumber::PAGE_SIZE != dest_off % PageNumber::PAGE_SIZE {
        todo!("support copy_ranges that aren't aligned")
    }
    let src_start = PageNumber::from_offset(src_off);
    let dest_start = PageNumber::from_offset(dest_off);

    let start_offset = src_off % PageNumber::PAGE_SIZE;
    let end_offset = (src_off + byte_length) % PageNumber::PAGE_SIZE;

    // Number of pages that will be touched, including partial pages.
    // By subtracting the partial pages, we are left with the full pages,
    // and then we can add in how many partial pages we'll be copying.
    let nr_pages: usize = byte_length.saturating_sub(start_offset + end_offset)
        / PageNumber::PAGE_SIZE
        + match (start_offset, end_offset) {
            (0, 0) => 0,
            (0, _) | (_, 0) => 1,
            (_, _) => 2,
        };
    logln!(
        "==> {:x} {:x} {:x} {:x} {:x} {} {}",
        src_off,
        dest_off,
        byte_length,
        start_offset,
        end_offset,
        byte_length / PageNumber::PAGE_SIZE,
        nr_pages
    );
    // Step 1: lock the page trees for the objects, in a canonical order.
    let (mut src_tree, mut dest_tree) = crate::utils::lock_two(&src.range_tree, &dest.range_tree);

    // Step 2: Invalidate the page ranges. In the destination, we fully unmap the object for that range. In the source,
    // we only need to ensure that no one modifies pages, so we just write-protect it.
    src.invalidate(
        src_start..src_start.offset(nr_pages),
        InvalidateMode::WriteProtect,
    );
    dest.invalidate(
        dest_start..dest_start.offset(nr_pages),
        InvalidateMode::Full,
    );

    // Step 3a: Copy any non-full-page at the start
    let mut dest_point = dest_start;
    let mut src_point = src_start;
    let mut remaining_pages = nr_pages;
    if start_offset != 0 {
        copy_single(
            &mut dest_tree,
            &mut src_tree,
            dest_point,
            src_point,
            start_offset,
            PageNumber::PAGE_SIZE,
        );
        dest_point = dest_point.offset(1);
        src_point = src_point.offset(1);
        remaining_pages -= 1;
    }

    // Step 3b: copy full pages. The number of pages is how many we have left, minus if we are going to do a partial page at the end.
    let vec_pages = remaining_pages - if end_offset > 0 { 1 } else { 0 };
    let mut remaining_vec_pages = vec_pages;
    if vec_pages > 0 {
        let ranges = src_tree.range(src_point..src_point.offset(vec_pages));
        for range in ranges {
            if src_point < *range.0 {
                /* TODO: we'll need to ensure all backing pages are present if we get here */
                let diff = *range.0 - src_point;
                dest_point = dest_point.offset(diff);
                remaining_vec_pages -= diff;
            }
            let offset = src_point.num().saturating_sub(range.0.num());
            let len = core::cmp::min(range.1.value().length - offset, remaining_vec_pages);
            copy_range_to_object_tree(&mut dest_tree, dest_point, range.1.value(), offset, len);
            dest_point = dest_point.offset(len);
            remaining_vec_pages -= len;
            src_point = src_point.offset(len);
        }
    }
    remaining_pages -= vec_pages;

    assert_eq!(remaining_pages == 1, end_offset > 0);
    assert!(remaining_pages == 1 || remaining_pages == 0);
    assert_eq!(remaining_vec_pages, 0);

    // Step 3c: Finally, copy the last partial page, if there is one.
    if end_offset > 0 {
        copy_single(
            &mut dest_tree,
            &mut src_tree,
            dest_point,
            src_point,
            0,
            end_offset,
        );
    }

    // TODO: remove this
    dest.invalidate(
        dest_start..dest_start.offset(nr_pages),
        InvalidateMode::Full,
    );
}

#[cfg(test)]
mod test {
    use twizzler_abi::{device::CacheType, object::Protections};

    use crate::{
        memory::context::{kernel_context, KernelMemoryContext, ObjectContextInfo},
        obj::{pages::Page, ObjectRef, PageNumber},
        userinit::create_blank_object,
    };

    use super::copy_ranges;

    fn copy_ranges_and_check(
        src: &ObjectRef,
        src_off: usize,
        dest: &ObjectRef,
        dest_off: usize,
        byte_length: usize,
    ) {
        copy_ranges(src, src_off, dest, dest_off, byte_length);

        let dko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
            dest.clone(),
            Protections::READ,
            CacheType::WriteBack,
        ));
        let dptr = dko.start_addr();

        let sko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
            src.clone(),
            Protections::READ,
            CacheType::WriteBack,
        ));
        let sptr = sko.start_addr();

        let src_slice = unsafe {
            core::slice::from_raw_parts(sptr.as_mut_ptr::<u8>().add(src_off), byte_length)
        };
        let dest_slice = unsafe {
            core::slice::from_raw_parts(dptr.as_mut_ptr::<u8>().add(dest_off), byte_length)
        };

        dest.invalidate(
            PageNumber::base_page()..PageNumber::base_page().offset(1000),
            crate::obj::InvalidateMode::Full,
        );
        //dest.print_page_tree();
        assert_eq!(src_slice.len(), dest_slice.len());
        //logln!("==> {:?}", src_slice);
        //logln!("==> {:?}", dest_slice);
        assert!(src_slice == dest_slice);
    }

    #[twizzler_kernel_macros::kernel_test]
    fn test_object_copy() {
        let src = create_blank_object();
        let dest = create_blank_object();

        for p in 0..254 {
            let mut tree: crate::mutex::LockGuard<'_, crate::obj::range::PageRangeTree> =
                src.lock_page_tree();
            tree.add_page(PageNumber::base_page().offset(p), Page::new());
            let (sp, _) = tree
                .get_page(PageNumber::base_page().offset(p), true)
                .unwrap();
            sp.as_mut_slice().fill((p + 1) as u8);
        }

        //src.print_page_tree();
        copy_ranges_and_check(&src, 0x1000, &dest, 0x1000, 0x1000);
        copy_ranges_and_check(&src, 0x3000, &dest, 0x2000, 0x1000);

        // overwrite
        //copy_ranges_and_check(&src, 0x2000, &dest, 0x1000, 0x1000);

        copy_ranges_and_check(&src, 0x3100, &dest, 0x4100, 0x1000);
        copy_ranges_and_check(&src, 0x5100, &dest, 0x5100, 0x100);
        copy_ranges_and_check(&src, 0x6100, &dest, 0x6100, 0x1300);
        copy_ranges_and_check(&src, 0x7800, &dest, 0x7800, 0x800);
        copy_ranges_and_check(&src, 0x8000, &dest, 0x8000, 0x800);
        copy_ranges_and_check(&src, 0x9000, &dest, 0x9000, 0x2100);
    }
}
