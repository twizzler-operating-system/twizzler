use core::sync::atomic::{AtomicU32, Ordering};

use super::{current_thread_ref, flags::THREAD_HAS_DONATED_PRIORITY, Thread};

#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriorityClass {
    Idle,
    Background,
    User,
    Realtime,
}

pub struct ThreadPriority {
    current: AtomicU32,
    donated: AtomicU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority {
    pub class: PriorityClass,
    pub value: u16,
}

pub const MAX_PRIORITY: u16 = 128;

impl Priority {
    pub const INTERRUPT: Self = Self {
        class: PriorityClass::Realtime,
        value: MAX_PRIORITY - 1,
    };
    pub const REALTIME: Self = Self {
        class: PriorityClass::Realtime,
        value: MAX_PRIORITY / 2,
    };
    pub const USER: Self = Self {
        class: PriorityClass::User,
        value: MAX_PRIORITY / 2,
    };
    pub const BACKGROUND: Self = Self {
        class: PriorityClass::Background,
        value: MAX_PRIORITY / 2,
    };
    pub const IDLE: Self = Self {
        class: PriorityClass::Idle,
        value: MAX_PRIORITY / 2,
    };

    pub fn from_raw(d: u32) -> Self {
        let class = match d >> 16 {
            0 => PriorityClass::Idle,
            1 => PriorityClass::Background,
            2 => PriorityClass::User,
            _ => PriorityClass::Realtime,
        };
        Self {
            class,
            value: (d & 0xffff) as u16,
        }
    }

    pub fn raw(&self) -> u32 {
        ((self.class as u32) << 16) | (self.value as u32)
    }
}

impl Thread {
    pub fn remove_donated_priority(&self) {
        if self
            .flags
            .fetch_and(!THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst)
            & THREAD_HAS_DONATED_PRIORITY
            != 0
        {
            self.donated_priority.store(u32::MAX, Ordering::SeqCst);
        }
    }

    pub fn get_donated_priority(&self) -> Option<Priority> {
        if self.flags.load(Ordering::SeqCst) & THREAD_HAS_DONATED_PRIORITY != 0 {
            let d = self.donated_priority.load(Ordering::SeqCst);
            if d == u32::MAX {
                None
            } else {
                Some(Priority::from_raw(d))
            }
        } else {
            None
        }
    }

    pub fn get_stable_effective_priority(&self) -> Priority {
        let raw = self.stable_priority.load(Ordering::Acquire);
        Priority::from_raw(raw)
    }

    pub fn stable_effective_priority(&self) -> Priority {
        let priority = self.effective_priority();
        self.stable_priority
            .store(priority.raw(), Ordering::Release);
        priority
    }

    pub fn effective_priority(&self) -> Priority {
        let priority = Priority::from_raw(self.priority.load(Ordering::SeqCst));
        if self.flags.load(Ordering::SeqCst) & THREAD_HAS_DONATED_PRIORITY != 0 {
            let donated_priority = Priority::from_raw(self.donated_priority.load(Ordering::SeqCst));
            return core::cmp::max(donated_priority, priority);
        }
        priority
    }

    pub fn donate_priority(&self, pri: Priority) -> bool {
        let current_priority = self.effective_priority();
        if let Some(current) = self.get_donated_priority() {
            if current > pri {
                return false;
            }
        }
        let needs_resched = pri > current_priority;
        self.donated_priority.store(pri.raw(), Ordering::SeqCst);
        self.flags
            .fetch_or(THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst);
        if needs_resched {
            if let Some(cur) = current_thread_ref() {
                if cur.id() == self.id() {
                    return true;
                }
            }
            self.maybe_reschedule_thread();
        }
        true
    }
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;

    #[kernel_test]
    fn test_priority() {
        assert!(Priority::REALTIME > Priority::USER);
        assert!(Priority::REALTIME > Priority::BACKGROUND);
        assert!(Priority::REALTIME > Priority::IDLE);
        assert!(Priority::USER > Priority::BACKGROUND);
        assert!(Priority::USER > Priority::IDLE);
        assert!(Priority::BACKGROUND > Priority::IDLE);

        let pri = Priority::BACKGROUND;
        let raw = pri.raw();
        let new_pri = Priority::from_raw(raw);
        assert_eq!(pri, new_pri);
    }

    #[kernel_test]
    fn test_priority_round_trip() {
        let test_cases = [
            Priority::REALTIME,
            Priority::USER,
            Priority::BACKGROUND,
            Priority {
                class: PriorityClass::Idle,
                value: 0,
            },
            Priority {
                class: PriorityClass::Idle,
                value: MAX_PRIORITY,
            },
            Priority {
                class: PriorityClass::Background,
                value: 0,
            },
            Priority {
                class: PriorityClass::Background,
                value: MAX_PRIORITY,
            },
            Priority {
                class: PriorityClass::User,
                value: 0,
            },
            Priority {
                class: PriorityClass::User,
                value: MAX_PRIORITY,
            },
            Priority {
                class: PriorityClass::Realtime,
                value: 0,
            },
            Priority {
                class: PriorityClass::Realtime,
                value: MAX_PRIORITY,
            },
        ];

        for original in test_cases {
            let raw = original.raw();
            let reconstructed = Priority::from_raw(raw);
            assert_eq!(
                original, reconstructed,
                "Round trip failed for {:?}",
                original
            );
        }
    }
}
