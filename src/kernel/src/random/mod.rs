pub mod cpu_trng;
mod fortuna;
mod jitter;

use alloc::{borrow::ToOwned, boxed::Box, sync::Arc, vec::Vec};
use core::{
    borrow::{Borrow, BorrowMut},
    cell::{Cell, RefCell},
    time::Duration,
};

use fortuna::{Accumulator, Contributor};
use rand_core::RngCore;

use crate::{
    mutex::{LockGuard, Mutex},
    once::Once,
    sched::schedule,
    spinlock::SpinLoop,
    syscall::sync::sys_thread_sync,
    thread::Thread,
};

const POLL_AMOUNT: usize = 64;

pub trait EntropySource {
    fn try_new() -> Result<Self, ()>
    where
        Self: Sized;
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error>;
}

struct EntropySources {
    sources: Vec<Mutex<(Box<dyn EntropySource>, Contributor)>>,
}

impl EntropySources {
    pub const fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    pub fn has_sources(&self) -> bool {
        self.source_count() != 0
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
    pub fn try_register_source<Source: EntropySource + 'static>(&mut self) -> Result<(), ()> {
        let source = Source::try_new()?;
        self.sources
            .push(Mutex::new((Box::new(source), Contributor::new())));
        Ok(())
    }

    pub fn contribute_entropy(&self, accumulator: &mut Accumulator) {
        let mut buf: [u8; POLL_AMOUNT] = [0u8; POLL_AMOUNT];
        for mut source in &self.sources {
            let mut source = source.lock();
            // add one event per source to each of the pools
            for _ in 0..fortuna::POOL_COUNT {
                if let Ok(_) = source.0.try_fill_entropy(&mut buf) {
                    accumulator.add_random_event(&mut source.1, &buf);
                }
            }
        }
    }
}

const ACCUMULATOR: Once<Mutex<Accumulator>> = Once::new();

/// Generates randomness and fills the out buffer with entropy.
///  
/// Will optionally block while waiting for entropy events.
///
/// Returns whether or not it successfully filled the out buffer with entropy
pub fn getrandom(out: &mut [u8], nonblocking: bool) -> bool {
    let acc = ACCUMULATOR;
    let mut acc: LockGuard<Accumulator> = acc.call_once(|| Mutex::new(Accumulator::new())).lock();
    let res = acc.borrow_mut().try_fill_random_data(out);
    if let Ok(()) = res {
        return true;
    }
    // try_fill_random_data only fails if unseeded
    // so the rest is trying to seed it/wait for it to be seeded
    if ENTROPY_SOURCES.has_sources() {
        ENTROPY_SOURCES.contribute_entropy(acc.borrow_mut());
        acc.try_fill_random_data(out)
            .expect("Should be seeded now & therefore shouldn't return an error");
    }
    if nonblocking {
        // doesn't block, returns false instead
        false
    } else {
        // otherwise schedule and recurse in again after this thread gets picked up again
        // this way it allows other work to get done, work that might result in entropy events
        drop(acc); // removes lock from the accumulator

        // block for 2 seconds and hope for other entropy-generating work to get done in the
        // meantime
        sys_thread_sync(&mut [], Some(&mut Duration::from_secs(2)));
        getrandom(out, true)
    }
}

/// Be sure to contribute at least one byte and at most 32 bytes.
pub fn contribute_entropy(
    contributor: &mut Contributor,
    event: &[u8],
) -> Result<(), self::fortuna::Error> {
    let acc = ACCUMULATOR;
    let mut acc = acc.call_once(|| Mutex::new(Accumulator::new())).lock();
    acc.add_random_event(contributor, event)
}

const ENTROPY_SOURCES: EntropySources = EntropySources::new();
pub fn register_entropy_source<T: EntropySource + 'static>() {
    // ignore whether or not the registration was successful
    let _: Result<(), ()> = ENTROPY_SOURCES.try_register_source::<T>();
}

fn contribute_entropy_regularly() {
    loop {
        crate::syscall::sync::sys_thread_sync(&mut [], Some(&mut Duration::from_secs(120))).expect(
            "shouldn't panic because sys_thread_sync doesn't panic if no ops are passed in",
        );
        let acc = ACCUMULATOR;
        let mut acc = acc.call_once(|| Mutex::new(Accumulator::new())).lock();
        ENTROPY_SOURCES.contribute_entropy(&mut acc)
    }
}
