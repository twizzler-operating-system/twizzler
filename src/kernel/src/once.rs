use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::processor::spin_wait_until;

pub struct Once<T> {
    status: AtomicU32,
    data: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: Once call_once has been issued, the underlying data structure is made available,
// and we internally manage consistency of the unsafecell and the status.
unsafe impl<T: Send + Sync> Sync for Once<T> {}
unsafe impl<T: Send> Send for Once<T> {}

const INCOMPLETE: u32 = 0;
const RUNNING: u32 = 1;
const COMPLETE: u32 = 2;

impl<T> Once<T> {
    /// Constructs a new Once with uninitialized data, must be initialized with call_once before it
    /// will return any data.
    pub const fn new() -> Self {
        Self {
            status: AtomicU32::new(INCOMPLETE),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    /// Initialize the data once and only once, returning the data once it is initialized. The given
    /// closure will only execute the first time this function is called, and otherwise will not be
    /// run.
    ///
    /// If multiple calls to call_once race, only one of them will run and initialize the data, the
    /// others will block.
    pub fn call_once<F: FnOnce() -> T>(&self, f: F) -> &T {
        let status = self.status.load(Ordering::SeqCst);
        if status == INCOMPLETE {
            match self.status.compare_exchange(
                INCOMPLETE,
                RUNNING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    // We willx initialize this Once.
                    // SAFETY: We are the only ones who can access the UnsafeCell, here, since we
                    // succeeded the cmpxchg above.
                    unsafe {
                        (*self.data.get()).as_mut_ptr().write(f());
                    }
                    self.status.store(COMPLETE, Ordering::SeqCst);
                }
                Err(_) => {
                    return self.wait();
                }
            }
        } else if status == RUNNING {
            return self.wait();
        }
        // SAFETY: Data will never change, since the status is COMPLETE, and the data is
        // initialized, for the same reason.
        return unsafe { self.force_get() };
    }

    unsafe fn force_get(&self) -> &T {
        &*(*self.data.get()).as_ptr()
    }

    /// If the data is not ready, then return None, or return Some if the data is ready. If this
    /// races with a call to call_once, the function will either return None or wait until the data
    /// is ready and return Some.
    pub fn poll(&self) -> Option<&T> {
        let status = spin_wait_until(
            || match self.status.load(Ordering::SeqCst) {
                COMPLETE => Some(COMPLETE),
                INCOMPLETE => Some(INCOMPLETE),
                _ => None,
            },
            || {},
        );

        if status == COMPLETE {
            // SAFETY: If status is complete, the data is ready.
            Some(unsafe { self.force_get() })
        } else {
            None
        }
    }

    /// Wait until the data is ready (someone calls call_once).
    pub fn wait(&self) -> &T {
        spin_wait_until(|| self.poll(), || {})
    }
}

impl<T> Drop for Once<T> {
    fn drop(&mut self) {
        // We don't have to check for running here, since we have &mut access to self.
        if self.status.load(Ordering::SeqCst) == COMPLETE {
            unsafe {
                core::ptr::drop_in_place((*self.data.get()).as_mut_ptr());
            }
        }
    }
}
