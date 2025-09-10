use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

pub const SAMPLE_PERIOD_TICKS: u64 = 1;

#[derive(Debug, Default)]
pub struct ThreadStats {
    pub user: AtomicU64,
    pub sys: AtomicU64,
    pub idle: AtomicU64,
    pub last: AtomicU64,
}

#[derive(Debug)]
pub struct ThreadSched {
    pub last_cpu: AtomicI32,
    pub pinned_cpu: AtomicI32,
    pub deadline: AtomicU64,
    pub sleep_tick: AtomicU64,
    pub current_processor_queue: AtomicI32,
    pub timeslice: AtomicU32,
}

impl Default for ThreadSched {
    fn default() -> Self {
        Self {
            last_cpu: AtomicI32::new(-1),
            pinned_cpu: AtomicI32::new(-1),
            deadline: AtomicU64::new(0),
            sleep_tick: AtomicU64::new(0),
            current_processor_queue: AtomicI32::new(-1),
            timeslice: AtomicU32::new(0),
        }
    }
}

impl ThreadSched {
    pub fn pin_cpu(&self, cpu: u32) {
        self.pinned_cpu.store(cpu as i32, Ordering::Release);
    }

    pub fn unpin_cpu(&self) {
        self.pinned_cpu.store(-1, Ordering::Release);
    }

    pub fn pinned_to(&self) -> Option<u32> {
        let cpu = self.pinned_cpu.load(Ordering::Acquire);
        if cpu >= 0 {
            Some(cpu as u32)
        } else {
            None
        }
    }

    pub fn pay_ticks(&self, ticks: u64, allowed: u64) -> bool {
        if self.timeslice.fetch_add(ticks as u32, Ordering::Acquire) as u64 + ticks >= allowed {
            self.timeslice.store(0, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn reset_timeslice(&self) {
        self.timeslice.store(0, Ordering::Release);
    }

    pub fn moving_to_queue(&self, cpu: u32) {
        self.current_processor_queue
            .store(cpu as i32, Ordering::Release);
    }

    pub fn moving_to_active(&self, cpu: u32) -> Option<u32> {
        self.current_processor_queue.store(-1, Ordering::Release);
        let old = self.last_cpu.swap(cpu as i32, Ordering::SeqCst);
        if old == -1 {
            None
        } else {
            Some(old as u32)
        }
    }

    pub fn current_cpu_rq(&self) -> Option<u32> {
        let cpu = self.current_processor_queue.load(Ordering::Acquire);
        if cpu >= 0 {
            Some(cpu as u32)
        } else {
            None
        }
    }

    /// Returns Some((cpu, pinned)), if either the thread is pinned or has a last cpu (in which case
    /// pinned = false).
    pub fn preferred_cpu(&self) -> Option<(u32, bool)> {
        let cpu = self.pinned_cpu.load(Ordering::Acquire);
        if cpu >= 0 {
            Some((cpu as u32, true))
        } else {
            let cpu = self.last_cpu.load(Ordering::Acquire);
            if cpu >= 0 {
                Some((cpu as u32, false))
            } else {
                None
            }
        }
    }

    pub fn set_deadline(&self, tick: u64) {
        self.deadline.store(tick, Ordering::Release);
    }

    pub fn get_deadline(&self) -> u64 {
        self.deadline.load(Ordering::Acquire)
    }
}
