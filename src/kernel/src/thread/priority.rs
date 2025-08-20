use core::{sync::atomic::Ordering, u16, u32};

use super::{current_thread_ref, flags::THREAD_HAS_DONATED_PRIORITY, Thread};
/// [`Thread`]s are triggered based on their priority, which is their [`PriorityClass`] coupled
/// with their adjustment number. Their
/// [`  PriorityClass`]
#[derive(Default, Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct Priority {
    raw: u32,
}

impl Priority {
    pub const REALTIME: Self = Self::new(PriorityClass::RealTime, 0);
    pub const USER: Self = Self::new(PriorityClass::User, 0);
    pub const BACKGROUND: Self = Self::new(PriorityClass::Background, 0);
    pub const IDLE: Self = Self::new(PriorityClass::Idle, 0);

    pub const fn new(class: PriorityClass, adjust: u16) -> Self {
        Self {
            raw: ((class.as_u16() as u32) << 16) | adjust as u32,
        }
    }

    pub fn from_raw(raw: u32) -> Self {
        if raw == u32::MAX {
            Self::IDLE
        } else {
            Self { raw }
        }
    }

    pub fn raw(&self) -> u32 {
        self.raw
    }

    pub fn class(&self) -> PriorityClass {
        let upper = (self.raw >> 16) as u16;
        PriorityClass::from(upper)
    }

    pub fn adjust(&self) -> u16 {
        self.raw as u16
    }

    pub fn queue_number<const NR_QUEUES: usize>(&self) -> usize {
        assert_eq!(NR_QUEUES % PriorityClass::ClassCount as usize, 0);
        let queues_per_class = NR_QUEUES / PriorityClass::ClassCount as usize;
        assert!(queues_per_class > 0);
        let base_queue = self.class() as usize * queues_per_class;
        let adj = self.adjust().clamp(0, (queues_per_class - 1) as u16);
        let q = base_queue + adj as usize;
        assert!(q < NR_QUEUES);
        q
    }

    pub fn from_queue_number<const NR_QUEUES: usize>(queue: usize) -> Self {
        if queue == NR_QUEUES {
            return Self::new(PriorityClass::Idle, u16::MAX);
        }
        let queues_per_class = NR_QUEUES / PriorityClass::ClassCount as usize;
        let class = queue / queues_per_class;
        assert!(class < PriorityClass::ClassCount as usize);
        let base_queue = class * queues_per_class;
        let adj = queue.saturating_sub(base_queue);
        Self::new(PriorityClass::from(class as u16), adj as u16)
    }
}

#[derive(Clone, Copy, PartialEq, Default, Debug, Eq)]
#[repr(u16)]
pub enum PriorityClass {
    /// Highest Priority
    RealTime = 0,
    /// Second highest priority
    User = 1,
    /// Third highest priority
    Background = 2,
    #[default]
    /// Lowest priority
    Idle = 3,
    ClassCount = 4,
}

impl From<PriorityClass> for u16 {
    fn from(value: PriorityClass) -> Self {
        value.as_u16()
    }
}

impl From<u16> for PriorityClass {
    fn from(value: u16) -> Self {
        match value {
            0 => Self::RealTime,
            1 => Self::User,
            2 => Self::Background,
            _ => Self::Idle,
        }
    }
}

impl PartialOrd for PriorityClass {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        /* backwards because of how priority works */
        (*other as u32).partial_cmp(&(*self as u32))
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        /* backwards because of how priority works */
        other.raw.partial_cmp(&self.raw)
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

impl PriorityClass {
    pub const fn as_u16(&self) -> u16 {
        match self {
            PriorityClass::RealTime => 0,
            PriorityClass::User => 1,
            PriorityClass::Background => 2,
            PriorityClass::Idle => 3,
            PriorityClass::ClassCount => 4,
        }
    }
}

impl Thread {
    pub fn remove_donated_priority(&self) {
        if self.get_donated_priority().is_some() {
            log::trace!("remove donated pri: {:?}", self.get_donated_priority());
        }
        self.donated_priority.store(u32::MAX, Ordering::SeqCst);
        self.flags
            .fetch_and(!THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst);
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

    #[inline]
    pub fn queue_number<const NR_QUEUES: usize>(&self) -> usize {
        self.effective_priority().queue_number::<NR_QUEUES>()
    }

    pub fn adjust_priority(&self, amount: i32) {
        let priority = Priority::from_raw(self.priority.load(Ordering::SeqCst));
        let new_priority = Priority::new(
            priority.class(),
            (priority.adjust() as i32 + amount).clamp(0, u16::MAX as i32) as u16,
        );
        self.priority.store(new_priority.raw(), Ordering::SeqCst);
    }
}

mod test {
    use core::u16;

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
    fn test_queue_number() {
        for i in 0..u16::MAX {
            let pri = Priority::new(PriorityClass::User, i);
            let q = pri.queue_number::<64>();

            let pri2 = Priority::new(PriorityClass::Background, i);
            let q2 = pri2.queue_number::<64>();
            assert!(q < q2);
        }
    }

    #[kernel_test]
    fn test_queue_number_roundtrip() {
        for i in 0..u16::MAX {
            for class in [
                PriorityClass::Idle,
                PriorityClass::Background,
                PriorityClass::RealTime,
                PriorityClass::User,
            ] {
                let pri = Priority::new(class, i);
                let q = pri.queue_number::<64>();
                let new_pri = Priority::from_queue_number::<64>(q);

                let q = new_pri.queue_number::<64>();
                let new_pri2 = Priority::from_queue_number::<64>(q);
                assert_eq!(new_pri, new_pri2);
            }
        }
    }
}
