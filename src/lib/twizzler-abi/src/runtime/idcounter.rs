//! Implements a simple unique reusable ID counter.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rustc_alloc::vec::Vec;

use crate::simple_mutex::Mutex;

/// A manager for IDs of size u32.
pub(crate) struct IdCounter {
    next: AtomicU32,
    stack_non_empty: AtomicBool,
    stack: Mutex<Vec<u32>>,
    uh_oh: AtomicBool,
}

// High watermark before falling back to full locking for the stack.
const MAX_BEFORE_UH_OH: usize = 128;

impl IdCounter {
    #[allow(dead_code)]
    /// Create a new IdCounter that will start on 0.
    pub const fn new_zero() -> Self {
        Self {
            next: AtomicU32::new(0),
            stack_non_empty: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            uh_oh: AtomicBool::new(false),
        }
    }

    /// Create a new IdCounter that will start on 1.
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

    /// Return a fresh ID, that is, either a new ID or one that has been previously released.
    pub fn fresh(&self) -> u32 {
        // Quickly check to see if we need to think about the stack.
        if self.stack_non_empty.load(Ordering::SeqCst) {
            // If the stack isn't too full, then only try to grab the lock.
            let dont_try_too_hard = !self.uh_oh.load(Ordering::SeqCst);
            if let Some(x) = if dont_try_too_hard {
                self.try_get_from_stack()
            } else {
                self.get_from_stack()
            } {
                // Got an old ID we can use.
                return x;
            }
        }

        // Next ID please!
        let next = self.next.fetch_add(1, Ordering::SeqCst);
        next
    }

    /// Release an ID to that it may be reused in the future. Note: it may not be immediately
    /// reused.
    pub fn release(&self, id: u32) {
        // First see if we can just subtract the next counter.
        if self
            .next
            .compare_exchange(id + 1, id, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
        // Okay, fine, we will lock.
        let mut stack = self.stack.lock();
        stack.push(id);
        if stack.len() > MAX_BEFORE_UH_OH {
            // We hit the high watermark, so make future calls to fresh() try harder to get the
            // lock.
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
        assert_eq!(ic.fresh(), 0)
    }

    #[test]
    fn test_new_one() {
        let ic = IdCounter::new_one();
        assert_eq!(ic.fresh(), 1)
    }

    #[test]
    fn test_fresh_simple() {
        let ic = IdCounter::new_zero();
        assert_eq!(ic.fresh(), 0);
        assert_eq!(ic.fresh(), 1);
        assert_eq!(ic.fresh(), 2);

        ic.release(1);
        assert_eq!(ic.fresh(), 1);

        ic.release(2);
        ic.release(1);
        assert_eq!(ic.fresh(), 1);
    }
}
