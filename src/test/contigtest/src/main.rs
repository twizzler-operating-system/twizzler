//! Exercises physically contiguous DMA pins (`DmaOptions::CONTIGUOUS`).

use std::time::Duration;

use twizzler::object::{ObjID, ObjectBuilder};
use twizzler_abi::{
    kso::{KactionCmd, KactionFlags, KactionGenericCmd, PinFlags, pack_kaction_pin_start_and_len},
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        DeleteFlags, ObjectControlCmd, PinnedPage, sys_kaction, sys_memory_stats, sys_object_ctrl,
    },
};
use twizzler_driver::dma::{
    Access, DMA_PAGE_SIZE, DmaObject, DmaOptions, DmaPin, DmaPool, DmaSliceRegion,
};
use twizzler_rt_abi::error::TwzError;

const SIZES: [usize; 6] = [1, 6, 15, 16, 64, 512];

fn addrs(pin: &DmaPin<'_>) -> Vec<u64> {
    pin.backing.iter().map(u64::from).collect()
}

fn is_contiguous(addrs: &[u64]) -> bool {
    addrs
        .iter()
        .enumerate()
        .all(|(i, a)| *a == addrs[0] + (i * DMA_PAGE_SIZE) as u64)
}

fn pattern(i: usize, seed: u64) -> u64 {
    (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ seed
}

fn check_pattern(reg: &mut DmaSliceRegion<u64>, seed: u64) {
    let len = reg.len();
    reg.with_mut(0..len, |s| {
        for (i, v) in s.iter_mut().enumerate() {
            *v = pattern(i, seed);
        }
    });
    reg.with(0..len, |s| {
        for (i, v) in s.iter().enumerate() {
            assert_eq!(*v, pattern(i, seed), "pattern mismatch at word {}", i);
        }
    });
}

fn new_dma_object() -> (DmaObject, ObjID) {
    let obj = ObjectBuilder::<()>::default().build(()).unwrap();
    let id = obj.id();
    (DmaObject::new(obj), id)
}

fn delete(id: ObjID) {
    let _ = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0);
}

fn words(pages: usize) -> usize {
    pages * DMA_PAGE_SIZE / size_of::<u64>()
}

fn raw_pin(id: ObjID, start: u64, len: usize, flags: u16) -> Result<Vec<u64>, TwzError> {
    let mut pins = vec![PinnedPage::new(0); len.max(1)];
    sys_kaction(
        KactionCmd::Generic(KactionGenericCmd::PinPages(flags)),
        Some(id),
        pins.as_mut_ptr() as u64,
        pack_kaction_pin_start_and_len(start, len).unwrap(),
        KactionFlags::empty(),
    )?;
    Ok(pins
        .iter()
        .take(len)
        .map(|p| p.physical_address())
        .collect())
}

fn test_pool() {
    let pool = DmaPool::new(
        DmaPool::default_spec(),
        Access::BiDirectional,
        DmaOptions::CONTIGUOUS,
    );
    // Twice: the second round reuses the ranges the first one freed.
    for round in 0..2 {
        for pages in SIZES {
            let mut reg = pool.allocate_array(words(pages), 0u64).unwrap();
            let a = addrs(&reg.pin().unwrap());
            assert_eq!(a.len(), pages);
            assert!(is_contiguous(&a), "pool {} pages: {:x?}", pages, a);
            check_pattern(&mut reg, pages as u64);
            println!(
                "contigtest: pool round {} pages {:3} base {:#x} contiguous ok",
                round, pages, a[0]
            );
        }
    }
    // Several live at once, so they cannot all be the same run.
    let mut regs: Vec<_> = SIZES
        .iter()
        .map(|pages| pool.allocate_array(words(*pages), 0u64).unwrap())
        .collect();
    for (reg, pages) in regs.iter_mut().zip(SIZES) {
        let a = addrs(&reg.pin().unwrap());
        assert!(is_contiguous(&a));
        check_pattern(reg, 0x55 + pages as u64);
    }
    assert!(pool.allocate_array(words(513), 0u64).is_err());
    println!("contigtest: pool live-set ok, 513 pages rejected");
}

fn test_object() {
    let (dma, id) = new_dma_object();
    let mut offset = NULLPAGE_SIZE;
    for pages in SIZES {
        let mut reg = dma.slice_region::<u64>(
            offset,
            words(pages),
            Access::BiDirectional,
            DmaOptions::CONTIGUOUS,
        );
        let a = addrs(&reg.pin().unwrap());
        assert_eq!(a.len(), pages);
        assert!(is_contiguous(&a), "object {} pages: {:x?}", pages, a);
        reg.with(0..words(pages), |s| assert!(s.iter().all(|v| *v == 0)));
        check_pattern(&mut reg, pages as u64);
        // A second pin of a populated contiguous range reports the same run.
        assert_eq!(
            raw_pin(id, (offset / DMA_PAGE_SIZE) as u64, pages, 1).unwrap(),
            a
        );
        println!(
            "contigtest: object pages {:3} base {:#x} contiguous+zeroed ok",
            pages, a[0]
        );
        offset += pages * DMA_PAGE_SIZE;
    }
    drop(dma);
    delete(id);
}

fn test_non_contiguous_and_errors() {
    let (dma, id) = new_dma_object();
    let base = NULLPAGE_SIZE / DMA_PAGE_SIZE;

    let mut reg = dma.slice_region::<u64>(
        NULLPAGE_SIZE,
        words(32),
        Access::BiDirectional,
        DmaOptions::empty(),
    );
    let a = addrs(&reg.pin().unwrap());
    assert_eq!(a.len(), 32);
    check_pattern(&mut reg, 7);
    println!(
        "contigtest: plain pin of 32 pages ok (happens to be contiguous: {})",
        is_contiguous(&a)
    );

    // Fully populated already: only succeeds if it happens to be contiguous.
    match raw_pin(id, base as u64, 32, PinFlags::CONTIGUOUS.bits()) {
        Ok(b) => {
            assert_eq!(a, b);
            assert!(is_contiguous(&b));
            println!("contigtest: contiguous pin over populated range ok (was contiguous)");
        }
        Err(e) => {
            assert!(!is_contiguous(&a));
            println!(
                "contigtest: contiguous pin over populated non-contiguous range: {}",
                e
            );
        }
    }
    assert_eq!(raw_pin(id, base as u64, 32, 0).unwrap(), a);
    reg.with(0..words(32), |s| {
        assert!(s.iter().enumerate().all(|(i, v)| *v == pattern(i, 7)))
    });

    // Partly populated: one page in the middle of an otherwise untouched range.
    let part = base as u64 + 1024;
    raw_pin(id, part + 3, 1, 0).unwrap();
    let e = raw_pin(id, part, 8, PinFlags::CONTIGUOUS.bits()).unwrap_err();
    println!(
        "contigtest: contiguous pin over partly populated range: {}",
        e
    );

    let fresh = base as u64 + 4096;
    let e = raw_pin(id, fresh, 513, PinFlags::CONTIGUOUS.bits()).unwrap_err();
    println!("contigtest: 513 pages: {}", e);
    let e = raw_pin(id, fresh, 0, PinFlags::CONTIGUOUS.bits()).unwrap_err();
    println!("contigtest: 0 pages: {}", e);
    let last = (MAX_SIZE / DMA_PAGE_SIZE) as u64;
    let e = raw_pin(id, last - 4, 8, PinFlags::CONTIGUOUS.bits()).unwrap_err();
    println!("contigtest: range past object end: {}", e);
    let e = raw_pin(id, u32::MAX as u64, 8, PinFlags::CONTIGUOUS.bits()).unwrap_err();
    println!("contigtest: start far past object end: {}", e);
    let e = raw_pin(id, fresh, 8, 0x8000).unwrap_err();
    println!("contigtest: unknown pin flag: {}", e);
    // None of the failures above may have populated anything.
    let a = raw_pin(id, fresh, 8, PinFlags::CONTIGUOUS.bits()).unwrap();
    assert!(is_contiguous(&a));
    println!("contigtest: range still pinnable after rejected requests");

    drop(reg);
    drop(dma);
    delete(id);
}

/// Frames available to allocate: on the free lists, or parked in a per-cpu cache.
fn free_frames() -> usize {
    let t = sys_memory_stats().tracker;
    t.idle + t.pooled
}

fn settle(target: usize, slack: usize) -> usize {
    let mut free = 0;
    for _ in 0..240 {
        twizzler_rt_abi::core::twz_rt_gc();
        std::thread::sleep(Duration::from_millis(250));
        free = free_frames();
        if free + slack >= target {
            break;
        }
    }
    free
}

fn churn(iters: usize) {
    for i in 0..iters {
        let pages = [6, 15, 16, 64][i % 4];
        let (dma, id) = new_dma_object();
        let mut reg = dma.slice_region::<u64>(
            NULLPAGE_SIZE,
            words(pages),
            Access::BiDirectional,
            DmaOptions::CONTIGUOUS,
        );
        let a = addrs(&reg.pin().unwrap());
        assert!(is_contiguous(&a));
        reg.with_mut(0..words(pages), |s| s[0] = i as u64);
        drop(reg);
        drop(dma);
        delete(id);
        if i % 64 == 63 {
            twizzler_rt_abi::core::twz_rt_gc();
        }
    }
}

fn test_leak() {
    const ITERS: usize = 4000;
    const SLACK: usize = 4096;
    // Warm every cache the loop fills, so the baseline is a steady state.
    churn(256);
    let before = settle(usize::MAX, 0);
    let stats = sys_memory_stats();
    println!(
        "contigtest: leak baseline: free {} frames, level-1 free {}",
        before, stats.levels[1].free_pages
    );
    churn(ITERS);
    let during = free_frames();
    let after = settle(before, SLACK / 4);
    let stats = sys_memory_stats();
    println!(
        "contigtest: after {} pin/delete cycles: free {} right after, {} settled ({:+} frames), level-1 free {}",
        ITERS,
        during,
        after,
        after as isize - before as isize,
        stats.levels[1].free_pages
    );
    assert!(after + SLACK >= before, "leaked {} frames", before - after);
}

fn main() {
    test_pool();
    test_object();
    test_non_contiguous_and_errors();
    test_leak();
    println!("contigtest: PASS");
}
