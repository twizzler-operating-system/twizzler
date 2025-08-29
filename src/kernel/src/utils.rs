use alloc::vec::Vec;
use core::fmt::Display;

use crate::{
    is_bench_mode,
    mutex::{LockGuard, Mutex},
    processor::mp::current_processor,
    spinlock::{self, GenericSpinlock, RelaxStrategy},
    time::bench_clock,
};

pub fn align<T: From<usize> + Into<usize>>(val: T, align: usize) -> T {
    let val = val.into();
    if val == 0 {
        return val.into();
    }
    let res: usize = ((val - 1) & !(align - 1)) + align;
    res.into()
}

/// Lock two mutexes in a stable order such that no deadlock cycles are created.
///
/// This is VITAL if you want to lock multiple mutexes for objects where you cannot
/// statically ensure ordering to avoid deadlock. It ensures that any two given mutexes
/// will be locked in the same order even if you permute the arguments to this function.
/// It does so by inspecting the addresses of the mutexes themselves to project a total
/// order onto the locks.
pub fn lock_two<'a, 'b, A, B>(
    a: &'a Mutex<A>,
    b: &'b Mutex<B>,
) -> (LockGuard<'a, A>, LockGuard<'b, B>) {
    let a_val = a as *const Mutex<A> as usize;
    let b_val = b as *const Mutex<B> as usize;
    assert_ne!(a_val, b_val);
    if a_val > b_val {
        let lg_b = b.lock();
        let lg_a = a.lock();
        (lg_a, lg_b)
    } else {
        let lg_a = a.lock();
        let lg_b = b.lock();
        (lg_a, lg_b)
    }
}
/// Lock two spinlocks in a stable order such that no deadlock cycles are created.
///
/// This is VITAL if you want to lock multiple mutexes for objects where you cannot
/// statically ensure ordering to avoid deadlock. It ensures that any two given spinlocks
/// will be locked in the same order even if you permute the arguments to this function.
/// It does so by inspecting the addresses of the spinlocks themselves to project a total
/// order onto the locks.
pub fn spinlock_two<'a, 'b, A, B, R: RelaxStrategy>(
    a: &'a GenericSpinlock<A, R>,
    b: &'b GenericSpinlock<B, R>,
) -> (spinlock::LockGuard<'a, A, R>, spinlock::LockGuard<'b, B, R>) {
    let a_val = a as *const GenericSpinlock<A, R> as usize;
    let b_val = b as *const GenericSpinlock<B, R> as usize;
    assert_ne!(a_val, b_val);
    if a_val > b_val {
        let lg_b = b.lock();
        let lg_a = a.lock();
        (lg_a, lg_b)
    } else {
        let lg_a = a.lock();
        let lg_b = b.lock();
        (lg_a, lg_b)
    }
}

#[thread_local]
static mut RAND_STATE: u32 = 0;

/// A quick, but poor, NON CRYPTOGRAPHIC random number generator.
pub fn quick_random() -> u32 {
    let mut state = unsafe { RAND_STATE };
    if state == 0 {
        state = current_processor().id;
    }
    let newstate = state.wrapping_mul(69069).wrapping_add(5);
    unsafe {
        RAND_STATE = newstate;
    }
    newstate >> 16
}

// benchmarking stuff
pub struct BenchResult {
    iterations: u64,
    total_ns: u64,
    avg_ns: f64,
    min_ns: u64,
    max_ns: u64,
}

impl Display for BenchResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "\nIterations: {}", self.iterations)?;
        writeln!(
            f,
            "Total time: {:.2} ms",
            self.total_ns as f64 / 1_000_000.0
        )?;
        writeln!(f, "Average:    {:.2} ns/iter", self.avg_ns)?;
        writeln!(f, "Min:        {:.2} ns/iter", self.min_ns)?;
        writeln!(f, "Max:        {:.2} ns/iter", self.max_ns)?;
        Ok(())
    }
}

fn benchmark_w_iter<F>(mut f: F, iterations: u64) -> BenchResult
where
    F: FnMut(),
{
    let mut times = Vec::with_capacity(iterations as usize);

    // warm up the bench
    for _ in 0..10 {
        f();
    }

    let clock = bench_clock().unwrap();

    for _ in 0..iterations {
        let start = clock.read();
        f();

        let end = clock.read();

        times.push(((end.value - start.value) * end.rate).as_nanos() as u64);
    }

    let total_ns: u64 = times.iter().sum();
    let avg_ns = total_ns as f64 / iterations as f64;
    let min_ns = *times.iter().min().unwrap();
    let max_ns = *times.iter().max().unwrap();

    BenchResult {
        iterations,
        total_ns,
        avg_ns,
        min_ns,
        max_ns,
    }
}

/// Benchmarks the closure, runs it for a scaled iteration count and
/// prints the results to console.
/// Limitation: you have to supply --tests as well for now to get
/// kernel benches to run.
pub fn benchmark<F>(mut f: F)
where
    F: FnMut(),
{
    if !is_bench_mode() {
        return;
    }
    let mut iterations = 100u64;
    // 1 second
    let target_duration_ns = 1_000_000_000_u64;

    let clock = bench_clock().unwrap();

    // scale till we figure out proper iterations
    loop {
        let start = clock.read();
        for _ in 0..iterations {
            f();
        }

        let end = clock.read();
        let duration = ((end.value - start.value) * end.rate).as_nanos() as u64;

        if duration >= target_duration_ns / 10 {
            iterations = (iterations * target_duration_ns) / duration;
            break;
        }

        iterations *= 10;

        // just in case
        if iterations > 10_000_000 {
            break;
        }
    }

    let res = benchmark_w_iter(f, iterations.min(10_000_000_u64));
    logln!("{}", res);
}
