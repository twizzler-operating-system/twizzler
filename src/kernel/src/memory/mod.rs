use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::{arch, instant::Instant, security::KERNEL_SCTX, spinlock::Spinlock, BootInfo};

pub mod allocator;
pub mod context;
pub mod frame;
pub mod pagetables;
pub mod tracker;

use alloc::vec::Vec;

pub use arch::{PhysAddr, VirtAddr};
use frame::NR_LEVELS;
use tracker::{alloc_frame, print_tracker_stats, reclaim, FrameAllocFlags};
use twizzler_abi::object::NULLPAGE_SIZE;

use self::context::{KernelMemoryContext, UserContext};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MemoryRegionKind {
    UsableRam,
    Reserved,
    BootloaderReserved,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    pub start: PhysAddr,
    pub length: usize,
    pub kind: MemoryRegionKind,
}

impl MemoryRegion {
    pub fn split(mut self, len: usize) -> Option<(MemoryRegion, MemoryRegion)> {
        let len = len.next_multiple_of(NULLPAGE_SIZE);
        if self.length <= len {
            return None;
        }
        let mut second = self;
        second.start = self.start.offset(len).ok()?;
        second.length -= len;
        self.length = len;
        Some((self, second))
    }
}

pub fn init(boot_info: &dyn BootInfo) {
    frame::init(boot_info.memory_regions());
    let kc = context::kernel_context();
    kc.switch_to(KERNEL_SCTX);
    kc.init_allocator();
    allocator::init(kc);
    // set flag to indicate that mm system is initalized
    MEM_INIT.store(true, Ordering::SeqCst);
}

static MEM_INIT: AtomicBool = AtomicBool::new(false);

/// Indicates if memory management has been initalized by the boot core.
pub fn is_init() -> bool {
    MEM_INIT.load(Ordering::SeqCst)
}

pub fn prep_smp() {
    let kc = context::kernel_context();
    kc.prep_smp();
}

pub fn sim_memory_pressure() {
    logln!("TEST -- simulating memory pressure");

    let alloc_frames = || {
        (0..4096)
            .map(|_| alloc_frame(FrameAllocFlags::WAIT_OK))
            .collect::<Vec<_>>()
    };
    const NUM_ITERS: usize = 1024;
    //let mut alloced = Vec::new();
    for i in 0..NUM_ITERS {
        logln!("iteration {} / {}", i, NUM_ITERS);
        print_tracker_stats();
        let frames = alloc_frames();
        //alloced.push(frames);
        reclaim(frames);
    }
}

struct FaultStats {
    count: [AtomicU32; NR_LEVELS],
    total: AtomicU64,
    time: Spinlock<Instant>,
}

static FAULT_STATS: FaultStats = FaultStats {
    count: [const { AtomicU32::new(0) }; NR_LEVELS],
    total: AtomicU64::new(0),
    time: Spinlock::new(Instant::zero()),
};

pub fn print_fault_stats() {
    let mut start = FAULT_STATS.time.lock();
    let dt = (Instant::now() - *start).as_millis();
    let small_count = FAULT_STATS
        .count
        .get(0)
        .map(|x| x.swap(0, Ordering::SeqCst))
        .unwrap_or(0);
    let med_count = FAULT_STATS
        .count
        .get(1)
        .map(|x| x.swap(0, Ordering::SeqCst))
        .unwrap_or(0);
    let big_count = FAULT_STATS
        .count
        .get(2)
        .map(|x| x.swap(0, Ordering::SeqCst))
        .unwrap_or(0);
    logln!(
        "mem: {:5}:{:5}:{:5} ({:5}) faults over {:5} ms ({:5}:{:5}:{:5} ({:5}) / ms) total {}",
        big_count,
        med_count,
        small_count,
        small_count + med_count + big_count,
        dt,
        big_count as u128 / dt,
        med_count as u128 / dt,
        small_count as u128 / dt,
        (small_count + med_count + big_count) as u128 / dt,
        FAULT_STATS.total.load(Ordering::SeqCst)
    );
    *start = Instant::now();
}
