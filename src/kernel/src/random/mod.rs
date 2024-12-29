pub mod cpu_trng;
mod fortuna;
mod jitter;

use alloc::{boxed::Box, vec::Vec};
use core::{borrow::BorrowMut, time::Duration};

use cpu_trng::maybe_add_cpu_entropy_source;
use fortuna::{Accumulator, Contributor};
use jitter::maybe_add_jitter_entropy_source;

use crate::{
    mutex::{LockGuard, Mutex},
    once::Once,
    syscall::sync::sys_thread_sync,
    thread::{entry::run_closure_in_new_thread, priority::Priority},
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
            for i in 0..fortuna::POOL_COUNT * 2 {
                if let Ok(_) = source.0.try_fill_entropy(&mut buf) {
                    accumulator
                        .add_random_event(&mut source.1, &buf)
                        .expect("event should be properly sized");
                }
            }
        }
        logln!("contributed entropy to pool");
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
    // return false;
    let mut acc: LockGuard<Accumulator> = ACCUMULATOR
        .call_once(|| Mutex::new(Accumulator::new()))
        .lock();
    let res = acc.borrow_mut().try_fill_random_data(out);
    if let Ok(()) = res {
        return true;
    }
    logln!("need to seed accumulator");
    // try_fill_random_data only fails if unseeded
    // so the rest is trying to seed it/wait for it to be seeded
    let mut entropy_sources = ENTROPY_SOURCES
        .call_once(|| Mutex::new(EntropySources::new()))
        .lock();
    if entropy_sources.has_sources() {
        logln!("has sources");
        entropy_sources.contribute_entropy(acc.borrow_mut());
        acc.try_fill_random_data(out)
            .expect("Should be seeded now & therefore shouldn't return an error");
        drop((entropy_sources, acc));
        return getrandom(out, nonblocking);
    }
    drop((entropy_sources, acc));
    if nonblocking {
        // doesn't block, returns false instead
        false
    } else {
        // otherwise schedule and recurse in again after this thread gets picked up again
        // this way it allows other work to get done, work that might result in entropy events

        // block for 2 seconds and hope for other entropy-generating work to get done in the
        // meantime
        logln!("recursing");
        // sys_thread_sync(&mut [], Some(&mut Duration::from_secs(2))).expect(
        //     "shouldn't panic because sys_thread_sync doesn't panic if no ops are passed in",
        // );
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
    let mut entropy_sources = ENTROPY_SOURCES
        .call_once(|| Mutex::new(EntropySources::new()))
        .lock();
    let res: Result<(), ()> = entropy_sources.try_register_source::<T>();
    res.is_ok()
}

pub fn start_entropy_contribution_thread() {
    // let thread = start_new_kernel(
    //     Priority::default_background(),
    //     contribute_entropy_regularly,
    //     0,
    // );
    let _registered_cpu = maybe_add_cpu_entropy_source();
    let _registered_jitter = maybe_add_jitter_entropy_source();
    // FIXME: currently this thread never is actually run again due to
    // default_background priority coupled with sys_thread_sync never actually
    // causing the thread to resume.
    run_closure_in_new_thread(Priority::default_user(), || contribute_entropy_regularly());
}

extern "C" fn contribute_entropy_regularly() {
    logln!("Starting entropy contribution loop; once every 100s");
    loop {
        crate::syscall::sync::sys_thread_sync(&mut [], Some(&mut Duration::from_secs(100))).expect(
            "shouldn't panic because sys_thread_sync doesn't panic if no ops are passed in",
        );
        let mut acc = ACCUMULATOR
            .call_once(|| Mutex::new(Accumulator::new()))
            .lock();
        let mut entropy_sources = ENTROPY_SOURCES
            .call_once(|| Mutex::new(EntropySources::new()))
            .lock();
        entropy_sources.contribute_entropy(&mut acc);
        drop(entropy_sources);
        drop(acc);
        // break;
    }
}

mod test {
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
