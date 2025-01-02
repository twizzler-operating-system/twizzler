use core::sync::atomic::{AtomicI32, Ordering};

use super::{current_thread_ref, flags::THREAD_HAS_DONATED_PRIORITY, Thread};
/// [`Thread`]s are triggered based on their priority, which is their [`PriorityClass`] coupled
/// with their adjustment number. Their
/// [`  PriorityClass`]
#[derive(Default, Debug)]
pub struct Priority {
    pub(super) class: PriorityClass,
    pub(super) adjust: AtomicI32,
}
impl Priority {
    #[allow(clippy::declare_interior_mutable_const)]
    pub const REALTIME: Self = Self {
        class: PriorityClass::RealTime,
        adjust: AtomicI32::new(0),
    };
    pub fn queue_number<const NR_QUEUES: usize>(&self) -> usize {
        assert_eq!(NR_QUEUES % PriorityClass::ClassCount as usize, 0);
        let queues_per_class = NR_QUEUES / PriorityClass::ClassCount as usize;
        assert!(queues_per_class > 0 && queues_per_class % 2 == 0);
        let equilibrium = (queues_per_class / 2) as i32;
        let base_queue = self.class as usize * queues_per_class + equilibrium as usize;
        let adj = self
            .adjust
            .load(Ordering::SeqCst)
            .clamp(-equilibrium, equilibrium);
        let q = ((base_queue as i32) + adj) as usize;
        assert!(q < NR_QUEUES);
        q
    }

    pub fn from_queue_number<const NR_QUEUES: usize>(queue: usize) -> Self {
        if queue == NR_QUEUES {
            return Self {
                class: PriorityClass::Idle,
                adjust: AtomicI32::new(i32::MAX),
            };
        }
        let queues_per_class = NR_QUEUES / PriorityClass::ClassCount as usize;
        let class = queue / queues_per_class;
        assert!(class < PriorityClass::ClassCount as usize);
        let equilibrium = queues_per_class / 2;
        let base_queue = class * queues_per_class + equilibrium;
        let adj = queue as i32 - base_queue as i32;
        Self {
            class: unsafe { core::intrinsics::transmute(class as u32) },
            adjust: AtomicI32::new(adj),
        }
    }

    pub fn default_user() -> Self {
        Self {
            class: PriorityClass::User,
            adjust: Default::default(),
        }
    }

    pub fn default_realtime() -> Self {
        Self {
            class: PriorityClass::RealTime,
            adjust: Default::default(),
        }
    }

    pub fn default_idle() -> Self {
        Self {
            class: PriorityClass::Idle,
            adjust: Default::default(),
        }
    }

    pub fn default_background() -> Self {
        Self {
            class: PriorityClass::Background,
            adjust: Default::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
#[repr(u32)]
pub(super) enum PriorityClass {
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

impl PartialEq for Priority {
    fn eq(&self, other: &Self) -> bool {
        self.class == other.class
            && self.adjust.load(Ordering::Relaxed) == other.adjust.load(Ordering::Relaxed)
    }
}

impl PartialOrd for PriorityClass {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        /* backwards because of how priority works */
        (*other as usize).partial_cmp(&(*self as usize))
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        match self.class.partial_cmp(&other.class) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        let thisadj = self.adjust.load(Ordering::Relaxed);
        let thatadj = other.adjust.load(Ordering::Relaxed);
        /* backwards because of how priority works */
        thatadj.partial_cmp(&thisadj)
    }
}

impl Clone for Priority {
    fn clone(&self) -> Self {
        Self {
            class: self.class,
            adjust: AtomicI32::new(self.adjust.load(Ordering::SeqCst)),
        }
    }
}

impl Eq for Priority {
    fn assert_receiver_is_total_eq(&self) {}
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        //is this okay?
        self.partial_cmp(other).unwrap()
    }
}

impl Thread {
    pub fn remove_donated_priority(&self) {
        let current_priority = self.effective_priority();
        let mut donated_priority = self.donated_priority.lock();
        self.flags
            .fetch_and(!THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst);
        *donated_priority = None;
        drop(donated_priority);
        if current_priority < self.effective_priority() {
            self.maybe_reschedule_thread();
        }
    }

    pub fn get_donated_priority(&self) -> Option<Priority> {
        let d = self.donated_priority.lock();
        (*d).clone()
    }

    pub fn effective_priority(&self) -> Priority {
        if self.flags.load(Ordering::SeqCst) & THREAD_HAS_DONATED_PRIORITY != 0 {
            let donated_priority = self.donated_priority.lock();
            if let Some(ref donated) = *donated_priority {
                return core::cmp::max(donated.clone(), self.priority.clone());
            }
        }
        self.priority.clone()
    }

    pub fn donate_priority(&self, pri: Priority) -> bool {
        let current_priority = self.effective_priority();
        let mut donated_priority = self.donated_priority.lock();
        if let Some(ref current) = *donated_priority {
            if current > &pri {
                return false;
            }
        }
        let needs_resched = pri > current_priority;
        *donated_priority = Some(pri);
        self.flags
            .fetch_or(THREAD_HAS_DONATED_PRIORITY, Ordering::SeqCst);
        drop(donated_priority);
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
        self.priority.queue_number::<NR_QUEUES>()
    }
}
