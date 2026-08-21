use twizzler_abi::{device::CacheType, object::Protections, syscall::MapFlags};

use crate::{
    memory::{
        context::{KernelMemoryContext, ObjectContextInfo, kernel_context},
        frame::PHYS_LEVEL_LAYOUTS,
        pagetables::Consistency,
        tracker::{FrameAllocFlags, FrameAllocator, alloc_frame},
    },
    obj::{ObjectRef, PageNumber},
    userinit::create_blank_object,
};

/// A volatile object with one large page mapped at `offset`.
///
/// `None` if no large frame is available, which is a fact about allocator state rather than about
/// anything under test -- a kernel test that fails halts the machine, so it must not assert on it.
fn object_with_large_page(offset: usize) -> Option<ObjectRef> {
    assert!(offset.is_multiple_of(PHYS_LEVEL_LAYOUTS[1].size()));
    let obj = create_blank_object();
    let mut alloc = FrameAllocator::new(
        FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
        PHYS_LEVEL_LAYOUTS[1],
    );
    let frame = alloc.try_allocate()?;
    assert_eq!(frame.size(), PHYS_LEVEL_LAYOUTS[1].size());
    obj.add_frame(PageNumber::from_offset(offset), frame);
    assert_eq!(
        obj.lock_page_tables().get_frame(offset as u64)?.size(),
        PHYS_LEVEL_LAYOUTS[1].size()
    );
    Some(obj)
}

/// A large page must survive being written through the object API.
///
/// `maybe_cow_at` runs on every write path -- including every atomic, which goes through
/// `with_ref` -- and `cow_copy` used to split a huge entry whether or not any frame was actually
/// COW. A large page therefore lasted exactly until something wrote to it.
#[twizzler_kernel_macros::kernel_test]
fn large_page_survives_a_write() {
    let region = PHYS_LEVEL_LAYOUTS[1].size();
    let Some(obj) = object_with_large_page(region) else {
        return;
    };
    let offset = region + PageNumber::PAGE_SIZE;

    obj.write_at(&0xabu8, offset).unwrap();
    let val: u8 = obj.read_at(offset).unwrap();
    assert_eq!(val, 0xab);
    // An atomic takes the same path, via `with_ref`.
    obj.read_atomic_64(region + 64).unwrap();

    assert_eq!(
        obj.lock_page_tables()
            .get_frame(region as u64)
            .unwrap()
            .size(),
        PHYS_LEVEL_LAYOUTS[1].size(),
        "large page was split by a write"
    );
}

/// Pinning across a large page must report each 4 KiB page's own address.
///
/// A large frame backs 512 offsets and reports the region base for all of them, so pinning used to
/// hand the same physical address to a device 512 times (behind an `assert_eq!(po, 0)` that fired
/// first).
#[twizzler_kernel_macros::kernel_test]
fn pin_over_a_large_page_reports_every_page() {
    let region = PHYS_LEVEL_LAYOUTS[1].size();
    let Some(obj) = object_with_large_page(region) else {
        return;
    };

    let count = 4;
    let (pages, _) = obj.pin(PageNumber::from_offset(region), count).unwrap();
    assert_eq!(pages.len(), count);
    for (i, pair) in pages.windows(2).enumerate() {
        assert_eq!(
            pair[1].physical_address(),
            pair[0].physical_address() + PageNumber::PAGE_SIZE as u64,
            "pinned page {} does not follow page {}",
            i + 1,
            i
        );
    }
}

/// `read_object` walks by frame, so a large page has to be sliced from the offset it reached rather
/// than from the frame's base.
#[twizzler_kernel_macros::kernel_test]
fn read_object_spans_a_large_page() {
    // Region 0, because `read_object` starts at the base page and stops at the first hole.
    let Some(obj) = object_with_large_page(0) else {
        return;
    };
    let base = PageNumber::PAGE_SIZE;
    obj.write_at(&0xcdu8, base).unwrap();
    obj.write_at(&0xefu8, base + 8).unwrap();

    let bytes = crate::operations::read_object(&obj);
    assert_eq!(bytes.len(), PHYS_LEVEL_LAYOUTS[1].size() - base);
    assert_eq!(bytes[0], 0xcd);
    assert_eq!(bytes[8], 0xef);
}

/// Splitting a large frame and freeing every piece must leave the level-0 free lists usable, and
/// should make the large frame available again.
///
/// The group counters that drive coalescing are maintained on exactly these transitions, so a
/// miscount or a bad unlink shows up here as corrupted free lists rather than as a subtle shortage.
#[twizzler_kernel_macros::kernel_test]
fn split_frames_free_and_reallocate() {
    use crate::memory::{
        frame::get_frame,
        tracker::{free_frame, try_alloc_split_frames},
    };

    let nr = PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size();
    let Some((head, len)) = try_alloc_split_frames(FrameAllocFlags::WAIT_OK, PHYS_LEVEL_LAYOUTS[1])
    else {
        return;
    };
    assert_eq!(len, PHYS_LEVEL_LAYOUTS[1].size());
    let base = head.start_address();

    for i in 0..nr {
        let frame = get_frame(base.offset(i * PHYS_LEVEL_LAYOUTS[0].size()).unwrap()).unwrap();
        assert_eq!(frame.size(), PHYS_LEVEL_LAYOUTS[0].size());
        free_frame(frame);
    }

    // The lists have to still hand out distinct frames afterwards.
    let mut frames = alloc::vec::Vec::new();
    for _ in 0..nr {
        frames.push(alloc_frame(FrameAllocFlags::WAIT_OK));
    }
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.size(), PHYS_LEVEL_LAYOUTS[0].size());
        for other in &frames[(i + 1)..] {
            assert_ne!(frame.start_address(), other.start_address());
        }
    }
    for frame in frames {
        free_frame(frame);
    }
}

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

    // Ranges that cross a level-1 (2 MiB) boundary. Every case above fits inside one level-1
    // region, which is why they all passed while `setup_zero_range` advanced its cursor twice per
    // region -- once in the child that walked it and once more in the parent -- and so zeroed only
    // the first region of any range spanning two. The straddling case is the cheap one to get
    // wrong: it loses a single page at the boundary rather than half the range.
    let l1 = PHYS_LEVEL_LAYOUTS[1].size();
    zero_ranges_and_check(&dest, l1 - ps, ps * 2);
    zero_ranges_and_check(&dest, l1 * 2, l1 * 2);
    zero_ranges_and_check(&dest, l1 * 4 + ps, l1 * 2 + ps);

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
/// creator's map would otherwise take it -- and `scan_deleted` runs from the bsp idle loop and
/// from `ObjectControlCmd::Delete`, neither of which this thread controls the timing of.
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

/// A sleeper must never be lost to `wakeup_word`'s lock-free early-out, and the count it tests
/// must return to zero so that the early-out actually engages.
///
/// The race this exists for is narrow and one-sided: a waker that reads `sleepers` as zero skips
/// the mutex entirely, so if the count is published *after* the sleeper commits to blocking rather
/// than before, the wake is dropped and the sleeper never runs again. That failure needs the waker
/// and the sleeper to interleave inside a few instructions, which is why this hammers a round trip
/// rather than testing one -- and why it asserts progress under a deadline instead of asserting a
/// state, since the symptom of the bug is a thread that stops rather than a value that is wrong.
///
/// The zero check is the other half. A count that leaked upward would be *safe* -- every wake just
/// takes the lock as before -- and therefore silent, leaving the optimization permanently disabled
/// with nothing to notice it. Asserting it drains is what keeps that from rotting.
#[twizzler_kernel_macros::kernel_test]
fn sleeper_count_wakes_and_drains() {
    use core::{sync::atomic::Ordering, time::Duration};

    use twizzler_abi::syscall::{
        ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
    };

    use crate::{
        syscall::sync::sys_thread_sync,
        thread::{entry::run_closure_in_new_thread, priority::Priority},
    };

    const ROUNDS: usize = 200;
    /// Generous: this is a liveness deadline, not a performance one. A lost wakeup parks the
    /// worker forever, so any finite bound catches it; the only job of the number is to fail
    /// rather than hang the machine.
    const LIMIT: Duration = Duration::from_secs(30);

    let obj = create_blank_object();
    let id = obj.id();
    // Page 1, not the null page: offset 0 is not writable object memory.
    let offset = PageNumber::base_page().as_byte_offset();
    // The worker's progress word. It exists so this thread can *block* waiting for the worker
    // instead of spinning on shared memory, which deadlocks whenever there is one cpu: tests run
    // at REALTIME (see `idle_main`, deliberately, so threads a test spawns cannot preempt it) and
    // the worker below is USER, and `RunQueue::take` serves realtime first with no aging between
    // classes. A realtime busy-wait therefore starves the exact thread it is waiting for. That is
    // what this test did originally, and it hung 6/6 at smp1 while passing at smp2 and smp4.
    let ack = offset + core::mem::size_of::<u64>();
    // Also faults the page in, so the worker's first read cannot fail. Both words share it.
    obj.swap_atomic_64(offset, 0).unwrap();
    obj.swap_atomic_64(ack, 0).unwrap();

    // Block until `word` reads at least `target`.
    let wait_for = |word: usize, target: u64| {
        loop {
            let cur = obj.read_atomic_64(word).unwrap();
            if cur >= target {
                return;
            }
            // Sleep only while the word still reads what was just seen, so an update landing
            // between the read and the sleep declines to block rather than being missed.
            let sleep = ThreadSyncSleep::new(
                ThreadSyncReference::ObjectRef(id, word),
                cur,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            );
            let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sleep)], None);
        }
    };

    let worker_obj = obj.clone();
    let (_thread, closure) = run_closure_in_new_thread(Priority::USER, move || {
        let obj = worker_obj;
        for round in 1..=ROUNDS as u64 {
            // Sleep while the word still reads the previous round's value. If the waker's store
            // beat us here, the op fails its check and we fall straight through, which is the
            // correct non-blocking outcome rather than a missed wake.
            let sleep = ThreadSyncSleep::new(
                ThreadSyncReference::ObjectRef(id, offset),
                round - 1,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            );
            while obj.read_atomic_64(offset).unwrap() < round {
                let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sleep)], None);
            }
            // Publish progress and wake the driver, which is blocked on this word.
            obj.try_write_val_and_signal(ack, round, usize::MAX)
                .unwrap();
        }
    });

    for round in 1..=ROUNDS as u64 {
        // Wait until the worker has finished the previous round, so this write races its next
        // sleep attempt rather than trivially preceding it -- the interleaving the early-out can
        // get wrong. Round 1 falls straight through, since `ack` already reads 0.
        wait_for(ack, round - 1);
        obj.try_write_val_and_signal(offset, round, usize::MAX)
            .unwrap();
    }

    assert!(
        closure.wait_timeout(LIMIT).is_some(),
        "worker did not finish {} sleep/wake rounds in {:?}: a wake was lost",
        ROUNDS,
        LIMIT
    );

    // Every park must have been released. Nonzero here means the fast path is dead for this
    // object -- safe, but silently pointless, which is the failure this half exists to catch.
    assert_eq!(
        // `tests` is a child of `obj`, so the private field is in scope here.
        obj.sleepers.load(Ordering::SeqCst),
        0,
        "sleeper count did not drain after {} rounds",
        ROUNDS
    );
}

/// `sys_object_copy`'s argument guards, which exist because most of what they reject panics the
/// kernel rather than failing the call.
///
/// A self-copy reaches `utils::lock_two`'s `assert_ne!`; an offset in the non-canonical hole
/// reaches `setup_zero_range`'s `VirtAddr::new(..).unwrap()`. A range that merely runs past the
/// object's end is the quiet one -- nothing panics, it just builds page-table entries outside the
/// object. The meta page falls under the same bound, being the object's last page, and holds the
/// `MetaInfo` that a content-derived id is computed over.
#[twizzler_kernel_macros::kernel_test]
fn object_copy_rejects_bad_ranges() {
    use twizzler_abi::object::MAX_SIZE;
    use twizzler_rt_abi::bindings::object_source;

    use crate::syscall::object::sys_object_copy;

    let dest = create_blank_object();
    let ps = PageNumber::PAGE_SIZE as u64;
    let meta = (MAX_SIZE - PageNumber::PAGE_SIZE) as u64;
    let zero_at = |dest_start, len| object_source {
        id: 0,
        src_start: 0,
        dest_start,
        len,
    };

    for (case, src) in [
        ("the meta page itself", zero_at(meta, ps)),
        (
            "a range running into the meta page",
            zero_at(meta - ps, ps * 2),
        ),
        (
            "a range past the end of the object",
            zero_at(meta + ps * 16, ps),
        ),
        (
            "an offset in the non-canonical hole",
            zero_at(1u64 << 47, ps),
        ),
        (
            "a length that overflows its offset",
            zero_at(u64::MAX - ps, ps * 2),
        ),
        (
            "an object copying from itself",
            object_source {
                id: dest.id().raw(),
                src_start: ps,
                dest_start: ps * 2,
                len: ps,
            },
        ),
    ] {
        assert!(
            sys_object_copy(dest.id(), &[src]).is_err(),
            "sys_object_copy accepted {}",
            case
        );
    }
}

/// What the syscall adds over `copy_range`/`zero_range`, which `test_object_copy` already covers:
/// a source with id 0 zeroes, any other id copies, and both kinds work in one call.
#[twizzler_kernel_macros::kernel_test]
fn object_copy_zeroes_and_copies() {
    use twizzler_rt_abi::bindings::object_source;

    use crate::syscall::object::sys_object_copy;

    let src = create_blank_object();
    let dest = create_blank_object();
    let ps = PageNumber::PAGE_SIZE;

    let frame = alloc_frame(FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK);
    unsafe { frame.as_byte_slice_mut().fill(0x5a) };
    src.add_frame(PageNumber::from_offset(ps), frame);
    dest.write_at(&0xffu8, ps * 2).unwrap();

    sys_object_copy(
        dest.id(),
        &[
            object_source {
                id: src.id().raw(),
                src_start: ps as u64,
                dest_start: ps as u64,
                len: ps as u64,
            },
            object_source {
                id: 0,
                src_start: 0,
                dest_start: (ps * 2) as u64,
                len: ps as u64,
            },
        ],
    )
    .unwrap();

    let copied: u8 = dest.read_at(ps).unwrap();
    assert_eq!(copied, 0x5a, "copy source did not land");
    let zeroed: u8 = dest.read_at(ps * 2).unwrap();
    assert_eq!(zeroed, 0, "zeroing source did not clear the page");
    let untouched: u8 = src.read_at(ps).unwrap();
    assert_eq!(untouched, 0x5a, "copy disturbed its source");
}
