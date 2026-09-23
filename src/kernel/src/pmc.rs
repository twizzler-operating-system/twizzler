//! One free-running LLC-miss counter per cpu, for the scheduler's cache-miss penalty (see
//! `sched-cache.patch` at the repo root for the FreeBSD original). Programmed at boot on every
//! cpu when the hardware has it; [read_local] is a single counter-read instruction and is safe
//! from interrupt and switch context. Deltas must go through [delta]: the counter is narrower
//! than 64 bits and wraps.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{arch::pmc as arch, once::Once};

/// Sweep constant: `false` leaves every cpu's PMU untouched.
pub const ENABLED: bool = true;

static PMU: Once<arch::Pmu> = Once::new();
static MASK: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Called on every cpu once it is up, bsp first (its probe decides for all cpus).
pub fn init_cpu() {
    if !ENABLED {
        return;
    }
    if crate::processor::mp::current_processor().is_bsp() {
        match arch::probe() {
            Ok(pmu) => {
                MASK.store(
                    if pmu.width >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << pmu.width) - 1
                    },
                    Ordering::Release,
                );
                logln!("[kernel::pmc] llc-miss counter: {} bits", pmu.width);
                PMU.call_once(|| pmu);
                ACTIVE.store(true, Ordering::Release);
                crate::thread::cachemiss::init();
            }
            Err(why) => logln!("[kernel::pmc] llc-miss counter unavailable: {}", why),
        }
    }
    if let Some(pmu) = PMU.poll() {
        arch::program_local(pmu);
    }
}

/// Whether every cpu is counting. False on hardware without a usable counter; callers then get
/// 0 from [read_local] and must apply no penalty.
#[inline]
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// The calling cpu's counter. Meaningful to compare only against another read from the same cpu,
/// so callers run with interrupts off or pinned.
#[inline]
pub fn read_local() -> u64 {
    if !is_active() {
        return 0;
    }
    arch::read_local()
}

/// Whether a counter read is a trap to a hypervisor rather than an instruction.
pub fn under_hypervisor() -> bool {
    arch::under_hypervisor()
}

/// Misses between two reads of one cpu's counter, correct across one wrap.
#[inline]
pub fn delta(prev: u64, now: u64) -> u64 {
    now.wrapping_sub(prev) & MASK.load(Ordering::Relaxed)
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    use crate::processor::mp::current_processor;

    #[kernel_test]
    fn test_delta_wraps() {
        let saved = MASK.swap(0xffff_ffff_ffff, Ordering::Relaxed);
        assert_eq!(delta(10, 15), 5);
        assert_eq!(delta(0xffff_ffff_fffe, 3), 5);
        MASK.store(saved, Ordering::Relaxed);
    }

    /// A random walk over a buffer larger than any LLC must move the counter. Skips, loudly,
    /// where there is no counter (TCG, or KVM without `pmu=on`), and if the test thread changed
    /// cpu between the two reads, since only same-cpu reads compare.
    #[kernel_test]
    fn test_llc_counter_counts_misses() {
        if !is_active() {
            logln!("[kernel::pmc] test skipped: no llc-miss counter");
            return;
        }
        let cpu = current_processor().id;
        const WORDS: usize = 4 << 20; // 32 MiB
        let mut buf = alloc::vec![0u64; WORDS];
        let before = read_local();
        let mut idx = 1usize;
        let mut sum = 0u64;
        for _ in 0..(1 << 20) {
            idx = idx
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407)
                & (WORDS - 1);
            sum = sum.wrapping_add(buf[idx]);
            buf[idx] = sum;
        }
        let after = read_local();
        core::hint::black_box((&buf, sum));
        if current_processor().id != cpu {
            logln!("[kernel::pmc] test skipped: thread migrated during the walk");
            return;
        }
        let misses = delta(before, after);
        logln!(
            "[kernel::pmc] 1M random accesses over 32 MiB: {} llc misses",
            misses
        );
        assert!(
            misses > 1000,
            "counter did not move: {} -> {}",
            before,
            after
        );
    }
}
