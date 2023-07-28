use core::sync::atomic::{AtomicI32, Ordering};

#[derive(Default, Debug)]
pub struct Priority {
    pub(super) class: PriorityClass,
    pub(super) adjust: AtomicI32,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
#[repr(u32)]
pub(super) enum PriorityClass {
    RealTime = 0,
    User = 1,
    Background = 2,
    #[default]
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
