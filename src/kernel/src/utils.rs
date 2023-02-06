use crate::{
    mutex::{LockGuard, Mutex},
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
