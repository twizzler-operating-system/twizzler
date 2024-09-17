pub mod cpu_trng;
mod fortuna;
mod jitter;

use alloc::{borrow::ToOwned, boxed::Box, sync::Arc, vec::Vec};
use core::{
    borrow::{Borrow, BorrowMut},
    cell::{Cell, RefCell},
    time::Duration,
};

use fortuna::{Accumulator, Contributor, MIN_POOL_SIZE};
use rand_core::RngCore;

use crate::{
    mutex::{LockGuard, Mutex},
    once::Once,
    sched::schedule,
    spinlock::SpinLoop,
    syscall::sync::sys_thread_sync,
    thread::{
        entry::{run_closure_in_new_thread, start_new_kernel},
        priority::Priority,
        Thread,
    },
};

const POLL_AMOUNT: usize = 64;

pub trait EntropySource {
    fn try_new() -> Result<Self, ()>
    where
        Self: Sized;
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error>;
}

struct EntropySources {
    sources: Vec<(Box<dyn EntropySource + Send + Sync>, Contributor)>,
}

impl EntropySources {
    pub fn new() -> Self {
        logln!("Created new entropy sources list");
        Self {
            sources: Vec::new(),
        }
    }

    pub fn has_sources(&self) -> bool {
        logln!("source count: {}", self.source_count());
        self.source_count() != 0
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
    pub fn try_register_source<Source: EntropySource + 'static + Send + Sync>(
        &mut self,
    ) -> Result<(), ()> {
        let source = Source::try_new()?;
        logln!("pushing source!");
        self.sources.push((Box::new(source), Contributor::new()));
        logln!("Sources count: {}", self.source_count());
        Ok(())
    }

    pub fn contribute_entropy(&mut self, accumulator: &mut Accumulator) {
        let mut buf: [u8; 32] = [0u8; 32];

        for source in &mut self.sources {
            // add two events per source to each of the pools
            // Two events because fortuna::MIN_POOL_SIZE is 64 bytes and each event
            // is restricted to be 32 bytes at most
            for _ in 0..fortuna::POOL_COUNT * 2 {
                if let Ok(_) = source.0.try_fill_entropy(&mut buf) {
                    accumulator
                        .add_random_event(&mut source.1, &buf)
                        .expect("event should be properly sized");
                }
            }
        }
    }
}

static ACCUMULATOR: Once<Mutex<Accumulator>> = Once::new();
static ENTROPY_SOURCES: Once<Mutex<EntropySources>> = Once::new();

/// Generates randomness and fills the out buffer with entropy.
///  
/// Will optionally block while waiting for entropy events.
///
/// Returns whether or not it successfully filled the out buffer with entropy
pub fn getrandom(out: &mut [u8], nonblocking: bool) -> bool {
    let mut acc: LockGuard<Accumulator> = ACCUMULATOR
        .call_once(|| {
            logln!("Calling call_once for acc in getrandom");
            Mutex::new(Accumulator::new())
        })
        .lock();
    logln!("filling random data");
    let res = acc.borrow_mut().try_fill_random_data(out);
    if let Ok(()) = res {
        return true;
    }
    logln!("need to seed accumulator");
    // try_fill_random_data only fails if unseeded
    // so the rest is trying to seed it/wait for it to be seeded
    let mut entropy_sources = ENTROPY_SOURCES
        .call_once(|| {
            logln!("Calling call_once for es in getrandom");
            Mutex::new(EntropySources::new())
        })
        .lock();
    if entropy_sources.has_sources() {
        logln!("has sources");
        entropy_sources.contribute_entropy(acc.borrow_mut());
        acc.try_fill_random_data(out)
            .expect("Should be seeded now & therefore shouldn't return an error");
    }
    drop(entropy_sources);
    if nonblocking {
        // doesn't block, returns false instead
        false
    } else {
        // otherwise schedule and recurse in again after this thread gets picked up again
        // this way it allows other work to get done, work that might result in entropy events
        drop(acc); // removes lock from the accumulator

        // block for 2 seconds and hope for other entropy-generating work to get done in the
        // meantime
        logln!("recursing");
        sys_thread_sync(&mut [], Some(&mut Duration::from_secs(2))).expect(
            "shouldn't panic because sys_thread_sync doesn't panic if no ops are passed in",
        );
        getrandom(out, nonblocking)
    }
}

/// Be sure to contribute at least one byte and at most 32 bytes.
pub fn contribute_entropy(
    contributor: &mut Contributor,
    event: &[u8],
) -> Result<(), self::fortuna::Error> {
    let mut acc = ACCUMULATOR
        .call_once(|| Mutex::new(Accumulator::new()))
        .lock();
    acc.add_random_event(contributor, event)
}
/// Returns whether registration was successful
pub fn register_entropy_source<T: EntropySource + 'static + Send + Sync>() -> bool {
    // ignore whether or not the registration was successful
    logln!("Registering entropy source");
    let mut entropy_sources = ENTROPY_SOURCES
        .call_once(|| Mutex::new(EntropySources::new()))
        .lock();
    let res: Result<(), ()> = entropy_sources.try_register_source::<T>();
    logln!("esc: {:?}", entropy_sources.sources.len());
    res.is_ok()
}

pub fn start_entropy_contribution_thread() {
    // let thread = start_new_kernel(
    //     Priority::default_background(),
    //     contribute_entropy_regularly,
    //     0,
    // );
    run_closure_in_new_thread(Priority::default_realtime(), || {
        contribute_entropy_regularly()
    });
}

extern "C" fn contribute_entropy_regularly() {
    logln!("Starting entropy contribution");
    loop {
        logln!("Contributing entropy");
        let mut acc = ACCUMULATOR
            .call_once(|| Mutex::new(Accumulator::new()))
            .lock();
        let mut entropy_sources = ENTROPY_SOURCES
            .call_once(|| Mutex::new(EntropySources::new()))
            .lock();
        entropy_sources.contribute_entropy(&mut acc);
        drop(entropy_sources);
        drop(acc);
        crate::syscall::sync::sys_thread_sync(&mut [], Some(&mut Duration::from_secs(120))).expect(
            "shouldn't panic because sys_thread_sync doesn't panic if no ops are passed in",
        );
    }
}

mod test {
    use cpu_trng::maybe_add_cpu_entropy_source;
    use jitter::maybe_add_jitter_entropy_source;
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    #[kernel_test]
    fn test_rand_gen() {
        let registered_jitter_entropy = maybe_add_jitter_entropy_source();
        let mut into = [0u8; 1024];
        logln!("jitter entropy registered: {}", registered_jitter_entropy);

        getrandom(&mut into, false);
        for byte in into {
            logln!("{}", byte);
        }
        // logln!("Into: {:?}", into)
    }
}
