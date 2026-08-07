use twizzler_abi::{device::CacheType, object::Protections, syscall::MapFlags};

use crate::{
    memory::{
        context::{KernelMemoryContext, ObjectContextInfo, kernel_context},
        pagetables::Consistency,
        tracker::{FrameAllocFlags, alloc_frame},
    },
    obj::{ObjectRef, PageNumber},
    userinit::create_blank_object,
};

fn check_slices(
    src: &ObjectRef,
    src_off: usize,
    dest: &ObjectRef,
    dest_off: usize,
    byte_length: usize,
) {
    let dko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
        dest.clone(),
        Protections::READ,
        CacheType::WriteBack,
        MapFlags::empty(),
    ));
    let dptr = dko.start_addr();

    let sko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
        src.clone(),
        Protections::READ,
        CacheType::WriteBack,
        MapFlags::empty(),
    ));
    let sptr = sko.start_addr();

    let src_slice =
        unsafe { core::slice::from_raw_parts(sptr.as_mut_ptr::<u8>().add(src_off), byte_length) };
    let dest_slice =
        unsafe { core::slice::from_raw_parts(dptr.as_mut_ptr::<u8>().add(dest_off), byte_length) };

    assert_eq!(src_slice.len(), dest_slice.len());
    if src_slice != dest_slice {
        sko.object().print_page_tree();
        dko.object().print_page_tree();
        panic!(
            "==> {} {} {:p} {:p}",
            src_slice[0],
            dest_slice[0],
            src_slice.as_ptr(),
            dest_slice.as_ptr()
        );
    }
    assert!(src_slice == dest_slice);
}

fn copy_ranges_and_check(
    src: &ObjectRef,
    src_off: usize,
    dest: &ObjectRef,
    dest_off: usize,
    byte_length: usize,
) {
    log::info!(
        "copy_ranges_and_check: src={:?} src_off={} dest={:?} dest_off={} byte_length={}",
        src.id(),
        src_off,
        dest.id(),
        dest_off,
        byte_length
    );
    src.copy_range(dest, src_off, dest_off, byte_length)
        .unwrap();
    check_slices(src, src_off, dest, dest_off, byte_length);
}

fn zero_ranges_and_check(dest: &ObjectRef, dest_off: usize, byte_length: usize) {
    log::info!(
        "zero_ranges_and_check: dest={:?} dest_off={:x} byte_length={:x}",
        dest.id(),
        dest_off,
        byte_length
    );
    {
        let dko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
            dest.clone(),
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MapFlags::empty(),
        ));
        let dptr = dko.start_addr();
        let dest_slice = unsafe {
            core::slice::from_raw_parts_mut(dptr.as_mut_ptr::<u8>().add(dest_off), byte_length)
        };
        dest_slice.fill(0xff);
        assert!(!dest_slice.iter().all(|x| *x == 0));
    }

    //dest.print_page_tree();
    dest.zero_range(dest_off, byte_length).unwrap();
    //dest.print_page_tree();

    let dko = kernel_context().insert_kernel_object::<u8>(ObjectContextInfo::new(
        dest.clone(),
        Protections::READ,
        CacheType::WriteBack,
        MapFlags::empty(),
    ));
    let dptr = dko.start_addr();
    let dest_slice =
        unsafe { core::slice::from_raw_parts(dptr.as_mut_ptr::<u8>().add(dest_off), byte_length) };
    Consistency::new_full_global();
    log::info!("checking zero in slice: {:p}", dest_slice.as_ptr(),);
    for (i, b) in dest_slice.iter().enumerate() {
        if *b != 0 {
            log::info!("found non-zero byte: at {:x} {:x}", i + dest_off, b);
        }
    }
    assert!(dest_slice.iter().all(|x| *x == 0));
}

#[twizzler_kernel_macros::kernel_test]
fn test_object_cow() {
    let src = create_blank_object();
    let dest = create_blank_object();

    // Skip the null page, otherwise fill the source with pages that have different fills
    for p in 1..255u8 {
        let pn = PageNumber::from_offset((p as usize) * PageNumber::PAGE_SIZE);
        let frame = alloc_frame(FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK);
        unsafe { frame.as_byte_slice_mut().fill(p) };
        src.add_frame(pn, frame);
    }

    let ps = PageNumber::PAGE_SIZE;
    copy_ranges_and_check(&src, ps, &dest, ps, ps * 250);
    let dc = dest
        .lock_page_tables()
        .maybe_cow_at((ps * 100) as u64, false)
        .unwrap();
    assert!(dc);
    dest.write_at(&0u8, ps * 100).unwrap();
    let val: u8 = dest.read_at(ps * 100).unwrap();
    assert_eq!(val, 0u8);
    let val: u8 = dest.read_at(ps * 100 + 1).unwrap();
    assert_eq!(val, 100u8);
    let val: u8 = src.read_at(ps * 100).unwrap();
    assert_eq!(val, 100u8);
}

#[twizzler_kernel_macros::kernel_test]
fn test_object_copy() {
    let src = create_blank_object();
    let dest = create_blank_object();

    // Skip the null page, otherwise fill the source with pages that have different fills
    for p in 1..255u8 {
        let pn = PageNumber::from_offset((p as usize) * PageNumber::PAGE_SIZE);
        let frame = alloc_frame(FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK);
        unsafe { frame.as_byte_slice_mut().fill(p) };
        src.add_frame(pn, frame);
    }

    let ps = PageNumber::PAGE_SIZE;
    let half_ps = PageNumber::PAGE_SIZE / 2;
    // This is for mis-aligning the offsets. Use about an eighth of a page for that, the exact
    // number doesn't matter.
    let abit = ps / 8;
    assert!(abit > 0 && abit < ps);

    // Some helper functions for finding regions of the objects to use for copy testing
    // automatically.
    let mut src_counting_page_num = 1;
    let mut dest_counting_page_num = 1;
    let calc_off = |page_num: usize, misalign: usize| -> usize { ps * page_num + misalign * abit };

    let mut do_check = |src_off_misalign, dest_off_misalign, len| {
        let nr_pages = len / PageNumber::PAGE_SIZE + 2; // Just bump up, assuming there are partial pages. Slightly wasteful, but it's just a
        // test.
        let src_off = calc_off(src_counting_page_num, src_off_misalign);
        let dest_off = calc_off(dest_counting_page_num, dest_off_misalign);
        src_counting_page_num += nr_pages;
        dest_counting_page_num += nr_pages;
        copy_ranges_and_check(&src, src_off, &dest, dest_off, len);
    };

    // Basic test
    do_check(0, 0, ps);

    // Overwrite. These two pages in src have different contents (see loop at start of this
    // function).
    let second_page = ps * 2;
    let third_page = ps * 3;
    copy_ranges_and_check(&src, second_page, &dest, second_page, ps);
    copy_ranges_and_check(&src, third_page, &dest, second_page, ps);

    // Misaligned, single page
    do_check(abit, abit, ps);
    // Misaligned, less than a page
    do_check(abit, abit, abit);
    // Misaligned, more than a page (but less than 2 pages)
    do_check(abit, abit, ps + abit);
    // Misaligned, at half page, for a half page (test boundary)
    do_check(half_ps, half_ps, half_ps);
    // Page aligned, less than a page
    do_check(0, 0, half_ps);
    // Page aligned, 2 pages and a bit more, not length aligned
    do_check(0, 0, ps * 2 + abit);

    // Test fallback to manual copy. Force that by doubling the partial page offset for dest,
    // but not src.
    do_check(abit, abit * 2, ps + abit);
    do_check(abit, abit * 2, abit);

    zero_ranges_and_check(&dest, ps, ps);
    // Test zeroing with a couple pages, not length aligned.
    zero_ranges_and_check(&dest, ps + abit, ps * 2 + abit);

    // Test two back-to-back ranges. This first copy will copy (page(2) + abit) -> (page(2) +
    // abit) for a len of a page. So the end point will be (page(3) + abit), which is
    // where the second copy starts.
    src.copy_range(&dest, second_page + abit, second_page + abit, ps)
        .unwrap();

    copy_ranges_and_check(&src, third_page + abit, &dest, third_page + abit, ps);
    // Make sure we didn't overwrite the first copy.
    check_slices(&src, second_page + abit, &dest, second_page + abit, ps);
}

/// An object created with `ObjectCreateFlags::DELETE` must survive until its creator has had a
/// chance to map it. The flag means "delete when the last mapping goes away", but a brand-new
/// object has no mappings, so a reap pass running between the create syscall returning and the
/// creator's map would otherwise take it -- and `scan_deleted` runs on every unmap syscall.
#[twizzler_kernel_macros::kernel_test]
fn delete_flagged_object_survives_until_mapped() {
    use twizzler_abi::syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags};

    use crate::{
        obj::{LookupFlags, LookupResult, lookup_object, scan_deleted},
        syscall::object::sys_object_create,
    };

    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::DELETE,
        Protections::all(),
    );
    let id = sys_object_create(&create, &[], &[]).unwrap();

    // Stand in for any other cpu unmapping something in this window.
    scan_deleted();

    assert!(
        matches!(
            lookup_object(id, LookupFlags::empty()),
            LookupResult::Found(_)
        ),
        "DELETE-flagged object {} was reaped before it was ever mapped",
        id
    );
}
