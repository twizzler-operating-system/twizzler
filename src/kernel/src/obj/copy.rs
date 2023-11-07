use crate::mutex::LockGuard;

use super::{
    pages::Page,
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
        // We need to split any kicked ranges into parts that don't overlap with new_range_key, and then reinsert those splits.
        let (r1, r2) = split_range(k.1, new_range_key.clone());
        if let Some(mut r1) = r1 {
            r1.gc_pagevec();
            let res = dest_tree.insert_replace(r1.start..r1.start.offset(r1.length), r1);
            assert!(res.is_empty());
        }
        if let Some(mut r2) = r2 {
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
    let (dest_page, _) = dest_tree.get_or_add_page(dest_point, true, |_, _| Page::new());
    if let Some((src_page, _)) = src_page {
        dest_page.as_mut_slice()[offset..max].copy_from_slice(&src_page.as_slice()[offset..max]);
    } else {
        // TODO: could skip this on freshly created page, if we can detect that. That's just an optimization, though.
        dest_page.as_mut_slice()[offset..max].fill(0);
    }
}

// Zero a single, partial page.
fn zero_single(
    dest_tree: &mut LockGuard<PageRangeTree>,
    dest_point: PageNumber,
    offset: usize,
    max: usize,
) {
    // if there's no page here, our work is done
    if let Some((dest_page, _)) = dest_tree.get_page(dest_point, true) {
        dest_page.as_mut_slice()[offset..max].fill(0);
    }
}
/// Copy page ranges from one object to another, preferring to share page vectors if possible.
///
/// In the case that a full page needs to be copied, it will likely be shared and set
/// to copy on write. In the case that a page needs to be partially copied, we'll do a manual copy
/// for that page. This only happens at the start and end of the copy region.
///
/// We allow non-page-aligned offsets, and that misalignment may differ between source and dest objects,
/// but the kernel may have to resort to a bytewise copy of the object pages if the offsets aren't both
/// misaligned by the same amount (e.g., if page size is 0x1000, then (dest off, src off) of (0x1000, 0x4000),
/// (0x1100, 0x3100) will still enable COW style copying, but (0x1100, 0x1200) will require manual copy).
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
    let src_start = PageNumber::from_offset(src_off);
    let dest_start = PageNumber::from_offset(dest_off);

    let start_offset = src_off % PageNumber::PAGE_SIZE;
    let end_offset = (src_off + byte_length) % PageNumber::PAGE_SIZE;
    let start_page_partial_len = if start_offset > 0 {
        PageNumber::PAGE_SIZE - start_offset
    } else {
        0
    };

    // Number of pages that will be touched, including partial pages.
    // By subtracting the partial pages, we are left with the full pages,
    // and then we can add in how many partial pages we'll be copying.
    let nr_pages: usize = byte_length.saturating_sub(start_page_partial_len + end_offset)
        / PageNumber::PAGE_SIZE
        + match (start_offset, end_offset) {
            (0, 0) => 0,
            (0, _) | (_, 0) => 1,
            (_, _) => 2,
        };
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

    // if we can't do COW copy, then fallback to full copy.
    if src_off % PageNumber::PAGE_SIZE != dest_off % PageNumber::PAGE_SIZE {
        copy_bytes(
            &mut src_tree,
            src_off,
            &mut dest_tree,
            dest_off,
            byte_length,
        );
        return;
    }

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
            // If the source point is below the range's start, then there's a hole in the source page tree. We don't have
            // to copy at all, just shift up the dest point to where it needs to be for this range (since we will be copying from it).
            if src_point < *range.0 {
                let diff = *range.0 - src_point;
                // If the hole is bigger than our copy region, just break.
                // Note: I don't think this will ever be true, given the way we select the ranges from the tree, but I haven't proven it yet.
                if diff > remaining_vec_pages {
                    dest_point = dest_point.offset(remaining_vec_pages);
                    remaining_vec_pages = 0;
                    break;
                }
                // TODO: we'll need to either ensure everything is present, or interface with the pager. We'll probably do the later in the future.
                dest_point = dest_point.offset(diff);
                remaining_vec_pages -= diff;
            }

            // Okay, finally, we can calculate the subrange from the source range that we'll be using for our destination region.
            let offset = src_point.num().saturating_sub(range.0.num());
            let len = core::cmp::min(range.1.value().length - offset, remaining_vec_pages);
            copy_range_to_object_tree(&mut dest_tree, dest_point, range.1.value(), offset, len);

            dest_point = dest_point.offset(len);
            src_point = src_point.offset(len);
            remaining_vec_pages -= len;
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
}

fn copy_bytes(
    src_tree: &mut PageRangeTree,
    src_off: usize,
    dest_tree: &mut PageRangeTree,
    dest_off: usize,
    byte_length: usize,
) {
    if byte_length > PageNumber::PAGE_SIZE * 3 {
        logln!(
            "warning -- copying many pages (~{}) manually due to misaligned copy-from directive",
            byte_length / PageNumber::PAGE_SIZE
        );
    }
    let src_start = PageNumber::from_offset(src_off);
    let dest_start = PageNumber::from_offset(dest_off);

    let mut src_point = src_start;
    let mut dest_point = dest_start;
    let mut remaining = byte_length;

    while remaining > 0 {
        let src_page = src_tree.get_page(src_point, false);
        let (dest_page, _) = dest_tree.get_or_add_page(dest_point, true, |_, _| Page::new());
        let count_sofar = byte_length - remaining;

        let this_src_offset = (src_off + count_sofar) % PageNumber::PAGE_SIZE;
        let this_dest_offset = (dest_off + count_sofar) % PageNumber::PAGE_SIZE;

        let this_length = if let Some((src_page, _)) = src_page {
            let this_length = core::cmp::min(
                core::cmp::min(
                    PageNumber::PAGE_SIZE - this_src_offset,
                    PageNumber::PAGE_SIZE - this_dest_offset,
                ),
                remaining,
            );
            dest_page.as_mut_slice()[this_dest_offset..(this_dest_offset + this_length)]
                .copy_from_slice(
                    &src_page.as_slice()[this_src_offset..(this_src_offset + this_length)],
                );
            this_length
        } else {
            let this_length = core::cmp::min(PageNumber::PAGE_SIZE - this_dest_offset, remaining);
            // TODO: could skip this on freshly created page, if we can detect that. That's just an optimization, though.
            dest_page.as_mut_slice()[this_dest_offset..(this_dest_offset + this_length)].fill(0);
            this_length
        };

        if this_src_offset + this_length >= PageNumber::PAGE_SIZE {
            src_point = src_point.offset(1);
        }
        if this_dest_offset + this_length >= PageNumber::PAGE_SIZE {
            dest_point = dest_point.offset(1);
        }
        remaining -= this_length;
    }
}

/// Zero a range of bytes in an object. The provided values need not be page-aligned.
/// The kernel will try to perform the zero-ing by writing as few zero bytes as it can,
/// preferring instead to delete page ranges and page vectors.
pub fn zero_ranges(dest: &ObjectRef, dest_off: usize, byte_length: usize) {
    let dest_start = PageNumber::from_offset(dest_off);

    let start_offset = dest_off % PageNumber::PAGE_SIZE;
    let end_offset = (dest_off + byte_length) % PageNumber::PAGE_SIZE;
    let start_page_partial_len = if start_offset > 0 {
        PageNumber::PAGE_SIZE - start_offset
    } else {
        0
    };

    // Number of pages that will be touched, including partial pages.
    // By subtracting the partial pages, we are left with the full pages,
    // and then we can add in how many partial pages we'll be copying.
    let nr_pages: usize = byte_length.saturating_sub(start_page_partial_len + end_offset)
        / PageNumber::PAGE_SIZE
        + match (start_offset, end_offset) {
            (0, 0) => 0,
            (0, _) | (_, 0) => 1,
            (_, _) => 2,
        };

    let mut dest_tree = dest.lock_page_tree();
    // Invalidate the destination object's range that's about to get zero'd.
    dest.invalidate(
        dest_start..dest_start.offset(nr_pages),
        InvalidateMode::Full,
    );

    let mut dest_point = dest_start;
    let mut remaining_pages = nr_pages;
    // Start with any first partial page.
    if start_offset != 0 {
        zero_single(
            &mut dest_tree,
            dest_point,
            start_offset,
            PageNumber::PAGE_SIZE,
        );
        dest_point = dest_point.offset(1);
        remaining_pages -= 1;
    }

    // Okay, now we'll try to evict page tree entries that comprise the region.
    let vec_pages = remaining_pages - if end_offset > 0 { 1 } else { 0 };
    if vec_pages > 0 {
        // Our plan is to collect all the page ranges within this range of pages, and remove them. We'll have to pay special attention
        // to the first and last ranges, though, as they may only partially overlap the region to be zero'd.
        let ranges = dest_tree.range(dest_point..dest_point.offset(vec_pages));
        let mut points = ranges
            .into_iter()
            .map(|r| r.0.clone())
            .collect::<alloc::vec::Vec<_>>();

        // Handle the last range, keeping only the parts that are after the zeroing region. We use pop because we
        // won't be needing to consider this entry later.
        if let Some(last) = points.pop() &&
            let Some(mut last_range) = dest_tree.remove(&last) {
                let last_point = dest_point.offset(vec_pages - 1);
                if last_point < last_range.start.offset(last_range.length) && last_point >= last_range.start {
                    let start_diff = last_point.offset(1) - last_range.start;
                    let len_diff = last_range.length - start_diff;

                    if last_range.length > len_diff {
                        last_range.length -= len_diff;
                        last_range.start = last_range.start.offset(start_diff);
                        last_range.offset += start_diff;
                        assert!(last_range.start == last_point.offset(1));
                        last_range.gc_pagevec();
                        let kicked = dest_tree.insert_replace(last_range.range(), last_range);
                        assert!(kicked.is_empty());
                    }
                }
        }

        // Handle the first range, truncating it if it starts before the zeroing region. Don't bother removing it from
        // the list -- we'll just skip it in the iterator (remove head of vec can be slow).
        if let Some(first) = points.first() &&
            let Some(mut first_range) = dest_tree.remove(first) {
                let first_point = dest_point;
                if first_point < first_range.start.offset(first_range.length) && first_point >= first_range.start {
                    let len_diff = first_range.start.offset(first_range.length) - first_point;

                    if first_range.length > len_diff {
                        first_range.length -= len_diff;
                        first_range.gc_pagevec();
                        let kicked = dest_tree.insert_replace(first_range.range(), first_range);
                        assert!(kicked.is_empty());
                    }
                }
        }

        // Finally we can remove the remaining ranges that are wholely contained. Skip the first one, though, we handled that above.
        for point in points.iter().skip(1) {
            dest_tree.remove(point);
        }
        dest_point = dest_point.offset(vec_pages);
    }
    remaining_pages -= vec_pages;

    assert_eq!(remaining_pages == 1, end_offset > 0);
    assert!(remaining_pages == 1 || remaining_pages == 0);

    if end_offset > 0 {
        zero_single(&mut dest_tree, dest_point, 0, end_offset);
    }
}

#[cfg(test)]
mod test {
    use twizzler_abi::{device::CacheType, object::Protections};

    use crate::{
        memory::context::{kernel_context, KernelMemoryContext, ObjectContextInfo},
        obj::{copy::zero_ranges, pages::Page, ObjectRef, PageNumber},
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

        assert_eq!(src_slice.len(), dest_slice.len());
        assert!(src_slice == dest_slice);
    }

    fn zero_ranges_and_check(dest: &ObjectRef, dest_off: usize, byte_length: usize) {
        {
            let dko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
                dest.clone(),
                Protections::READ,
                CacheType::WriteBack,
            ));
            let dptr = dko.start_addr();
            let dest_slice = unsafe {
                core::slice::from_raw_parts_mut(dptr.as_mut_ptr::<u8>().add(dest_off), byte_length)
            };
            dest_slice.fill(0xff);
            assert!(!dest_slice.iter().all(|x| *x == 0));
        }

        zero_ranges(dest, dest_off, byte_length);

        let dko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
            dest.clone(),
            Protections::READ,
            CacheType::WriteBack,
        ));
        let dptr = dko.start_addr();
        let dest_slice = unsafe {
            core::slice::from_raw_parts(dptr.as_mut_ptr::<u8>().add(dest_off), byte_length)
        };
        assert!(dest_slice.iter().all(|x| *x == 0));
    }

    #[twizzler_kernel_macros::kernel_test]
    fn test_object_copy() {
        let src = create_blank_object();
        let dest = create_blank_object();

        for p in 0..254u8 {
            let mut tree: crate::mutex::LockGuard<'_, crate::obj::range::PageRangeTree> =
                src.lock_page_tree();
            let (sp, _) =
                tree.get_or_add_page(PageNumber::base_page().offset(p as usize), true, |_, _| {
                    Page::new()
                });
            sp.as_mut_slice().fill(p + 1);
        }

        let ps = PageNumber::PAGE_SIZE;
        let abit = ps / 8;
        assert!(abit > 0 && abit < ps);

        // Basic test
        copy_ranges_and_check(&src, ps, &dest, ps, ps);

        // Overwrite
        copy_ranges_and_check(&src, ps * 2, &dest, ps * 2, ps);
        copy_ranges_and_check(&src, ps * 3, &dest, ps, ps);

        // Misaligned, single page
        copy_ranges_and_check(&src, ps * 4 + abit, &dest, ps * 4 + abit, ps);
        // Misaligned, less than a page
        copy_ranges_and_check(&src, ps * 5 + abit, &dest, ps * 5 + abit, abit);
        // Misaligned, more than a page (but less than 2 pages)
        copy_ranges_and_check(&src, ps * 6 + abit, &dest, ps * 6 + abit, ps + abit * 3);
        // Misaligned, at half page, for a half page (test boundary)
        copy_ranges_and_check(&src, ps * 7 + ps / 2, &dest, ps * 8 + ps / 2, ps / 2);
        // Page aligned, less than a page
        copy_ranges_and_check(&src, ps * 8, &dest, ps * 8, ps / 2);
        // Page aligned, more than 1 page, not length aligned
        copy_ranges_and_check(&src, ps * 9, &dest, ps * 9, ps * 2 + abit);

        // Test fallback to manual copy
        copy_ranges_and_check(&src, ps * 10 + abit, &dest, ps * 10 + abit * 2, ps + abit);
        copy_ranges_and_check(&src, ps * 11 + abit, &dest, ps * 12 + abit * 2, abit);

        zero_ranges_and_check(&dest, ps * 100, ps);
        zero_ranges_and_check(&dest, ps * 101 + abit, ps * 3 + abit * 3);
    }
}
