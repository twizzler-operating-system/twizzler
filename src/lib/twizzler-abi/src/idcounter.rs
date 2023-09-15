use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rustc_alloc::vec::Vec;

use crate::simple_mutex::Mutex;

pub(crate) struct IdCounter {
    next: AtomicU32,
    stack_non_empty: AtomicBool,
    stack: Mutex<Vec<u32>>,
    uh_oh: AtomicBool,
}

const MAX_BEFORE_UH_OH: usize = 128;

impl IdCounter {
    #[allow(dead_code)]
    pub const fn new_zero() -> Self {
        Self {
            next: AtomicU32::new(0),
            stack_non_empty: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            uh_oh: AtomicBool::new(false),
        }
    }

    pub const fn new_one() -> Self {
        Self {
            next: AtomicU32::new(1),
            stack_non_empty: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            uh_oh: AtomicBool::new(false),
        }
    }

    fn get_from_stack(&self) -> Option<u32> {
        self.stack.lock().pop()
    }

    fn try_get_from_stack(&self) -> Option<u32> {
        self.stack.try_lock()?.pop()
    }

    pub fn fresh(&self) -> u32 {
        if self.stack_non_empty.load(Ordering::SeqCst) {
            let dont_try_too_hard = !self.uh_oh.load(Ordering::SeqCst);
            if let Some(x) = if dont_try_too_hard {
                self.try_get_from_stack()
            } else {
                self.get_from_stack()
            } {
                return x;
            }
            self.stack_non_empty.store(false, Ordering::SeqCst);
        }

        let next = self.next.fetch_add(1, Ordering::SeqCst);
        next
    }

    pub fn release(&self, id: u32) {
        if self
            .next
            .compare_exchange(id + 1, id, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
        let mut stack = self.stack.lock();
        stack.push(id);
        if stack.len() > MAX_BEFORE_UH_OH {
            self.uh_oh.store(true, Ordering::SeqCst);
        }
        self.stack_non_empty.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_new_zero() {
        let ic = IdCounter::new_zero();
        assert_eq!(id.fresh(), 0)
    }

    fn test_new_one() {
        let ic = IdCounter::new_one();
        assert_eq!(id.fresh(), 1)
    }

    fn test_fresh_simple() {
        let ic = IdCounter::new_zero();
        assert_eq!(id.fresh(), 0);
        assert_eq!(id.fresh(), 1);
        assert_eq!(id.fresh(), 2);

        ic.release(1);
        assert_eq!(id.fresh(), 1);

        ic.release(2);
        ic.release(1);
        assert_eq!(id.fresh(), 1);
    }
}
