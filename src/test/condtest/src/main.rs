//! Stress mlibc's pthread condition variables (a lost wake hangs) and count the wake syscalls a
//! signal with no waiter costs.

use std::{
    cell::UnsafeCell,
    sync::Arc,
    time::{Duration, Instant},
};

use twizzler_abi::syscall::{sys_syscall_stats, Syscall};

struct Shared {
    mutex: UnsafeCell<libc::pthread_mutex_t>,
    cond: UnsafeCell<libc::pthread_cond_t>,
    value: UnsafeCell<u64>,
}

unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            mutex: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER),
            cond: UnsafeCell::new(libc::PTHREAD_COND_INITIALIZER),
            value: UnsafeCell::new(0),
        })
    }

    fn lock(&self) {
        assert_eq!(unsafe { libc::pthread_mutex_lock(self.mutex.get()) }, 0);
    }

    fn unlock(&self) {
        assert_eq!(unsafe { libc::pthread_mutex_unlock(self.mutex.get()) }, 0);
    }

    fn wait(&self) {
        assert_eq!(
            unsafe { libc::pthread_cond_wait(self.cond.get(), self.mutex.get()) },
            0
        );
    }

    fn timedwait(&self, rel: Duration) -> i32 {
        let mut now: libc::timespec = unsafe { core::mem::zeroed() };
        unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
        let ns = now.tv_nsec as u64 + rel.subsec_nanos() as u64;
        let abs = libc::timespec {
            tv_sec: now.tv_sec + rel.as_secs() as libc::time_t + (ns / 1_000_000_000) as libc::time_t,
            tv_nsec: (ns % 1_000_000_000) as _,
        };
        unsafe { libc::pthread_cond_timedwait(self.cond.get(), self.mutex.get(), &abs) }
    }

    fn signal(&self) {
        assert_eq!(unsafe { libc::pthread_cond_signal(self.cond.get()) }, 0);
    }

    fn broadcast(&self) {
        assert_eq!(unsafe { libc::pthread_cond_broadcast(self.cond.get()) }, 0);
    }

    fn value(&self) -> &mut u64 {
        unsafe { &mut *self.value.get() }
    }
}

/// Two threads take turns: each waits for the value to have the other's parity.
fn pingpong(rounds: u64) {
    let s = Shared::new();
    let t = {
        let s = s.clone();
        std::thread::spawn(move || {
            for i in 0..rounds {
                s.lock();
                while *s.value() != 2 * i + 1 {
                    s.wait();
                }
                *s.value() += 1;
                s.signal();
                s.unlock();
            }
        })
    };
    for i in 0..rounds {
        s.lock();
        while *s.value() != 2 * i {
            s.wait();
        }
        *s.value() += 1;
        s.signal();
        s.unlock();
    }
    t.join().unwrap();
    assert_eq!(*s.value(), 2 * rounds);
}

/// Waiters sleep until the generation moves; each broadcast must wake all of them.
fn broadcast(waiters: usize, generations: u64) {
    let s = Shared::new();
    let seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let threads: Vec<_> = (0..waiters)
        .map(|_| {
            let s = s.clone();
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut last = 0;
                s.lock();
                while last < generations {
                    while *s.value() == last {
                        s.wait();
                    }
                    last = *s.value();
                    seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                s.unlock();
            })
        })
        .collect();
    for g in 1..=generations {
        s.lock();
        *s.value() = g;
        s.broadcast();
        s.unlock();
        if g % 64 == 0 {
            std::thread::yield_now();
        }
    }
    for t in threads {
        t.join().unwrap();
    }
    assert!(seen.load(std::sync::atomic::Ordering::Relaxed) >= waiters as u64);
}

/// Timed waiters that mostly time out, against a signaler that sometimes arrives in time.
fn timed(rounds: u64) {
    let s = Shared::new();
    let t = {
        let s = s.clone();
        std::thread::spawn(move || {
            let mut timeouts = 0;
            for i in 0..rounds {
                s.lock();
                while *s.value() <= i {
                    if s.timedwait(Duration::from_micros(200)) == libc::ETIMEDOUT {
                        timeouts += 1;
                    }
                }
                s.unlock();
            }
            timeouts
        })
    };
    for _ in 0..rounds {
        std::thread::sleep(Duration::from_micros(if rand_bit() { 50 } else { 400 }));
        s.lock();
        *s.value() += 1;
        s.signal();
        s.unlock();
    }
    let timeouts = t.join().unwrap();
    println!("condtest: timed rounds={} timeouts={}", rounds, timeouts);
}

fn rand_bit() -> bool {
    Instant::now().elapsed().subsec_nanos() & 1 == 1
}

fn thread_syncs() -> usize {
    sys_syscall_stats().nr_syscalls_per_type[Syscall::ThreadSync as usize]
}

fn main() {
    let start = Instant::now();
    pingpong(100_000);
    println!("condtest: pingpong ok ({:?})", start.elapsed());

    let start = Instant::now();
    broadcast(8, 20_000);
    println!("condtest: broadcast ok ({:?})", start.elapsed());

    timed(2_000);

    // System-wide counter: other threads add noise, so report rather than assert exactly.
    let s = Shared::new();
    let before = thread_syncs();
    for _ in 0..100_000 {
        s.signal();
    }
    let after = thread_syncs();
    println!("condtest: 100000 unwaited signals made {} ThreadSync calls", after - before);
    println!("condtest: PASS");
}
