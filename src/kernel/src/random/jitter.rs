use rand_core::RngCore;
// see https://docs.rs/rand_jitter/0.4.0/rand_jitter/struct.JitterRng.html#example
use rand_jitter::JitterRng;

use super::{register_entropy_source, EntropySource};
use crate::{instant::Instant, time::TICK_SOURCES};
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
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.0.try_fill_bytes(dest)
    }
}

pub fn maybe_add_cpu_entropy_source() {
    register_entropy_source::<Jitter>()
}
