use crate::{
    mutex::{LockGuard, Mutex},
    processor::current_processor,
    spinlock::{self, GenericSpinlock, RelaxStrategy},
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
