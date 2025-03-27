use rand_core::TryRngCore;
// see https://docs.rs/rand_jitter/0.4.0/rand_jitter/struct.JitterRng.html#example
use rand_jitter::JitterRng;

use super::{register_entropy_source, EntropySource};
use crate::time::TICK_SOURCES;
pub fn get_nstime() -> u64 {
    let ticks = { TICK_SOURCES.lock()[0].read() };
    let span = ticks.value * ticks.rate;
    span.as_nanos() as u64
}

pub struct Jitter(JitterRng<fn() -> u64>);

impl EntropySource for Jitter {
    fn try_new() -> Result<Self, ()> {
        let mut jrng: JitterRng<fn() -> u64> = JitterRng::new_with_timer(get_nstime);
        let rounds = jrng.test_timer().or_else(|e| {
            logln!("Failed to instantiate Jitter: {}", e);
            Err(())
        })?;
        jrng.set_rounds(rounds);
        Ok(Jitter(jrng))
    }
    // shouldn't ever fail
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), ()> {
        self.0.try_fill_bytes(dest).map_err(|_| ())
    }
}

pub fn maybe_add_jitter_entropy_source() -> bool {
    register_entropy_source::<Jitter>()
}
