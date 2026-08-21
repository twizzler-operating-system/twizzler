mod error;
mod internal;
mod pool;
use alloc::vec::Vec;
use core::{
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

pub use error::Error;
use internal::{Generator, MAX_GEN_SIZE};
use pool::Pool;

use crate::instant::Instant;

// 9.5.5
pub(super) const MIN_POOL_SIZE: usize = 64;

/// 9.5.5: minimum wall-clock gap between reseeds, so that a burst of requests cannot drain the
/// pools faster than entropy arrives to refill them.
const MIN_RESEED_GAP: Duration = Duration::from_millis(100);

// based on Cryptography Engineering Chapter 9 by Neils Ferguson et. al.
// comments including 9.x.x reference the above text's sections

pub(super) const POOL_COUNT: usize = 32;

static CONTRIBUTOR_ID: AtomicU8 = AtomicU8::new(0);
// 9.5.6 utility class to make it easier to keep track of
// incrementing the pool number and make assigning ids easier as well.
pub struct Contributor {
    id: u8,
    pool_number: u8,
}

impl Contributor {
    pub fn new() -> Self {
        let out = Contributor {
            id: CONTRIBUTOR_ID.fetch_add(1, Ordering::Relaxed),
            pool_number: 0,
        };
        out
    }
    pub(self) fn contribute(&mut self) -> (u8, u8) {
        self.pool_number += 1;
        if self.pool_number > 32 {
            self.pool_number = 1;
        }
        (self.id, self.pool_number - 1)
    }
}

// 9.5.4
pub struct Accumulator {
    generator: Generator,
    reseed_ct: usize,
    pools: [Pool; POOL_COUNT],
    last_reseed_timestamp: Instant,
}

impl Accumulator {
    // 9.5.4
    pub fn new() -> Accumulator {
        let mut pools: Vec<Pool> = Vec::new();
        for _ in 0..POOL_COUNT {
            pools.push(Pool::new());
        }
        Accumulator {
            generator: Generator::new(),
            reseed_ct: 0,
            pools: pools
                .try_into()
                .expect("Vec should have the correct number of elements"),
            last_reseed_timestamp: Instant::zero(),
        }
    }

    pub fn is_seeded(&self) -> bool {
        self.reseed_ct != 0
    }

    // 9.5.5
    pub fn try_fill_random_data(&mut self, out: &mut [u8]) -> Result<(), self::error::Error> {
        let now = Instant::now();
        // Both halves of this were wrong against the comment they carried. 9.5.5 reseeds when
        // *more* than the gap has elapsed; the test was inverted, so this reseeded during bursts
        // and never when calls were spaced out. And the timestamp was assigned on every call
        // rather than on every reseed, so `diff` measured the gap between `getrandom`s rather than
        // between reseeds -- which is what the field name claims it is.
        //
        // The initial seed worked only *because* of the inversion: the first call saw a huge
        // `diff`, declined to reseed, fell through to `contribute_entropy`, and the recursive retry
        // then had a small enough `diff` to pass. So flipping the comparison alone hangs the boot
        // -- the first call would stamp the timestamp, the retry would fail the gap test, nothing
        // would ever seed, and `getrandom` recurses without sleeping. Assigning only on an actual
        // reseed is what keeps the retry able to seed.
        let diff = now - self.last_reseed_timestamp;
        if self.pools[0].count() >= MIN_POOL_SIZE && diff >= MIN_RESEED_GAP {
            self.last_reseed_timestamp = now;
            self.reseed_ct += 1;
            let mut all_pools = [0u8; (32 * POOL_COUNT)];
            let all_pools_iterator = all_pools.chunks_mut(32);
            let mut powered = 0b1;
            for (pool, buf) in self.pools.iter_mut().zip(all_pools_iterator) {
                if self.reseed_ct % powered == 0 {
                    pool.result(buf);
                } else {
                    break;
                }
                powered <<= 1;
            }
            self.generator.reseed(&all_pools);
        }
        if self.reseed_ct == 0 {
            return Err(self::error::Error::Unseeded);
        }

        for chunk in out.chunks_mut(MAX_GEN_SIZE) {
            self.generator.generate_random_data(chunk);
        }

        Ok(())
    }
    // 9.5.6 Add an event
    /// `source_number` is a unique id representing where the event is being contributed from.
    /// `pool_number` should be an 8 bit looping counter which input programs increment themselves
    /// to indicate which entropy bucket the event should be placed in.
    /// Be sure to contribute at least one byte and at most 32 bytes.
    pub fn add_random_event(
        &mut self,
        contributor: &mut Contributor,
        data: &[u8],
    ) -> Result<(), Error> {
        let (source_number, pool_number) = contributor.contribute();
        if data.len() < 1 {
            return Err(Error::TooLittleData);
        }
        if data.len() > 32 {
            return Err(Error::TooMuchData);
        }
        if pool_number > (POOL_COUNT - 1) as u8 {
            return Err(Error::PoolNumTooBig);
        }
        self.pools[pool_number as usize].insert(&[source_number, data.len() as u8]);
        self.pools[pool_number as usize].insert(data);
        Ok(())
    }
}

/// Bytes generated per refill of a [`PerCpuRng`] buffer.
///
/// `Generator::generate_random_data` rekeys after every request (9.4.4, two extra blocks) and
/// `generate_blocks` builds a fresh ChaCha20 key schedule per call, so a 16-byte nonce costs three
/// key schedules and three block generations. Batching amortises both over the batch. 512 rather
/// than something larger because the buffer holds generated-but-unhanded-out keystream: that is
/// not a forward-secrecy loss (the key has already rotated) but it is memory a disclosure bug
/// could read, so the window is kept small and the consumed prefix is zeroed as it is handed out.
pub(super) const BATCH: usize = 512;

/// A per-cpu generator plus its batch buffer. See [`BATCH`].
pub(crate) struct PerCpuRng {
    generator: Generator,
    buf: [u8; BATCH],
    /// Bytes of `buf` already handed out. `buf[..pos]` is zeroed.
    pos: usize,
    /// Bytes of `buf` that hold keystream. `pos <= filled <= BATCH`.
    filled: usize,
    /// Which [`super::SEED_GEN`] value `gen` was seeded at; 0 means never seeded.
    seed_gen: u64,
}

impl PerCpuRng {
    pub(crate) fn new() -> Self {
        Self {
            generator: Generator::new(),
            buf: [0; BATCH],
            pos: 0,
            filled: 0,
            seed_gen: 0,
        }
    }

    pub(crate) fn seed_gen(&self) -> u64 {
        self.seed_gen
    }

    /// Reseed from `seed` and discard any keystream from the previous seed.
    ///
    /// Discarding rather than serving the remainder is not a correctness requirement -- the old
    /// bytes are as good as the new ones -- but it makes "every byte in `buf` came from
    /// `seed_gen`" a stateable invariant, and it wipes the buffer, which is the point.
    pub(crate) fn reseed(&mut self, seed: &[u8; 32], generation: u64) {
        self.buf.fill(0);
        self.pos = 0;
        self.filled = 0;
        self.generator.reseed(seed);
        self.seed_gen = generation;
    }

    /// Assert the buffer's structural invariants.
    ///
    /// Test-only by intent: it scans the consumed prefix, which would be absurd per request on the
    /// hot path. `assert!` rather than `debug_assert!` so it cannot silently vanish if a release
    /// build ever calls it.
    pub(crate) fn check_invariants(&self) {
        assert!(
            self.pos <= self.filled,
            "pos {} exceeds filled {}",
            self.pos,
            self.filled
        );
        assert!(self.filled <= BATCH, "filled {} exceeds BATCH", self.filled);
        assert!(
            self.buf[..self.pos].iter().all(|b| *b == 0),
            "a byte already handed out is still in the buffer"
        );
    }

    /// Fill `out` from the batch buffer, refilling as needed. `out.len()` may exceed [`BATCH`].
    pub(crate) fn fill(&mut self, out: &mut [u8]) {
        debug_assert_ne!(self.seed_gen, 0, "fill on an unseeded PerCpuRng");
        // A request larger than the buffer bypasses it entirely: buffering would cost an extra
        // copy and gain no amortisation.
        if out.len() > BATCH {
            self.generator.generate_random_data(out);
            return;
        }
        let mut off = 0;
        while off < out.len() {
            if self.pos == self.filled {
                self.generator.generate_random_data(&mut self.buf);
                self.pos = 0;
                self.filled = BATCH;
            }
            let n = (out.len() - off).min(self.filled - self.pos);
            out[off..off + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            // Zeroed as consumed, so `buf[..pos]` never holds a byte a caller already has.
            self.buf[self.pos..self.pos + n].fill(0);
            self.pos += n;
            off += n;
        }
    }
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;

    fn seeded(byte: u8) -> PerCpuRng {
        let mut rng = PerCpuRng::new();
        rng.reseed(&[byte; 32], 1);
        rng
    }

    /// No request ever re-serves bytes a previous one received.
    ///
    /// This is the invariant that matters and the one no plausibility check can see. A cursor that
    /// fails to advance, or a refill overlapping the unconsumed tail, yields output of the right
    /// length, non-zero, with the right distribution -- and `new_nonce` (`syscall/object.rs`)
    /// turns that into duplicate object nonces, which is silent identity collision in a system
    /// that derives object identity from them. So the assertion is distinctness across the whole
    /// sequence, held here rather than derived from any counter the buffer keeps about itself.
    ///
    /// **Measured, and this test has never been shown to fail.** Two attempts ran the suite with
    /// the cursor deliberately frozen; both died in `test_mutex` before reaching here -- once
    /// under a peer's CPU load, once on an exclusively-held quiet box, so contention was not the
    /// cause. Across the evening: `test_mutex` passed in 4 of 4 clean boots and panicked in 2 of 2
    /// poisoned ones.
    ///
    /// The likely mechanism is the hazard this test names, arriving somewhere else first:
    /// `Object::new_kernel` draws its nonce from `getrandom`, a frozen cursor hands every object
    /// the same 16 bytes, and `Mutex`'s owner check compares ObjIDs -- so two threads with
    /// colliding control-object ids make the owner test false-positive, which is exactly the
    /// `this mutex is not re-entrant` panic observed. Consistent with the evidence and not
    /// directly observed; no duplicate ids were printed.
    ///
    /// So: the regression guard for a frozen cursor is the boot, not this test. What this pins is
    /// a cursor bug mild enough to leave the system running -- which is the version that would
    /// otherwise reach production behind a green suite.
    #[kernel_test]
    fn percpu_rng_never_reserves_a_byte() {
        let mut rng = seeded(0x5a);
        // 16 bytes is `new_nonce`'s request size, and BATCH/16 = 32, so N spans eight refills.
        const N: usize = 256;
        let mut seen: Vec<[u8; 16]> = Vec::with_capacity(N);
        for _ in 0..N {
            let mut out = [0u8; 16];
            rng.fill(&mut out);
            rng.check_invariants();
            seen.push(out);
        }
        seen.sort_unstable();
        for pair in seen.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "the generator handed out the same 16 bytes twice"
            );
        }
    }

    /// Refill boundaries, straddling requests, and requests larger than the buffer.
    ///
    /// Sizes chosen to land on and across `BATCH`: 511 and 513 bracket it, 31 and 7 leave the
    /// cursor at awkward offsets so later requests straddle a refill.
    #[kernel_test]
    fn percpu_rng_mixed_sizes_keep_invariants() {
        let mut rng = seeded(0x17);
        let sizes = [1usize, 16, 31, 64, 511, 512, 513, 7];
        let mut buf = [0u8; 1024];
        for _round in 0..8 {
            for &n in sizes.iter() {
                rng.fill(&mut buf[..n]);
                rng.check_invariants();
            }
        }
    }

    /// A reseed discards the previous seed's keystream rather than serving the remainder.
    ///
    /// Asserted as "the buffer holds no keystream", which is the property, rather than as a
    /// cursor value that would merely be consistent with it.
    #[kernel_test]
    fn percpu_rng_reseed_wipes_the_buffer() {
        let mut rng = seeded(0x01);
        let mut out = [0u8; 16];
        rng.fill(&mut out);
        // Control: without a partially-drained buffer the assertion below is vacuous.
        assert!(
            rng.pos > 0 && rng.pos < BATCH,
            "control failed: buffer was not left partially drained (pos {})",
            rng.pos
        );
        rng.reseed(&[0x02; 32], 2);
        assert_eq!(rng.pos, 0);
        assert_eq!(rng.filled, 0);
        assert!(
            rng.buf.iter().all(|b| *b == 0),
            "reseed left keystream from the previous seed in the buffer"
        );
    }

    /// Fill pool 0 past `MIN_POOL_SIZE`. A `Contributor` cycles its pool number, so every 32nd
    /// event lands in pool 0; 64 events puts two there, i.e. 68 bytes.
    fn fill_pool_zero(acc: &mut Accumulator, c: &mut Contributor) {
        for i in 0..64u8 {
            acc.add_random_event(c, &[i; 32]).unwrap();
        }
    }

    /// The first request after entropy arrives must seed.
    ///
    /// This pins the regression the inverted gap test was hiding. With `diff < GAP`, a first call
    /// sees an enormous `diff`, declines to reseed, and only `getrandom`'s recursive retry seeds
    /// the accumulator. Flipping the comparison *without* moving the timestamp assignment inside
    /// the reseed branch reproduces that failure permanently -- and `getrandom` recurses without
    /// sleeping, so it is a boot hang rather than a slow path.
    #[kernel_test]
    fn test_first_request_after_entropy_seeds() {
        let mut acc = Accumulator::new();
        let mut c = Contributor::new();
        assert!(!acc.is_seeded());
        fill_pool_zero(&mut acc, &mut c);
        // Control: if the event-to-pool arithmetic above is wrong, pool 0 never reaches the
        // threshold and the assertion below would pass for the wrong reason -- an unseeded
        // accumulator that declines for lack of entropy looks identical to one that declines
        // because the gap test is inverted.
        assert!(
            acc.pools[0].count() >= MIN_POOL_SIZE,
            "test setup failed to fill pool 0: {} < {}",
            acc.pools[0].count(),
            MIN_POOL_SIZE
        );
        let mut out = [0u8; 32];
        acc.try_fill_random_data(&mut out)
            .expect("first request after entropy must seed");
        assert!(acc.is_seeded());
    }

    /// A request that *fails* must not consume the reseed gap.
    ///
    /// The invariant: `last_reseed_timestamp` names the last *reseed*, so an attempt that reseeds
    /// nothing must leave it alone. Asserted as behaviour a caller can observe rather than as the
    /// field it depends on.
    ///
    /// **This test cannot observe the defect it was written for, and that is worth stating rather
    /// than leaving implied.** It was added believing it discriminated the naive repair -- flip
    /// `<` to `>=` but leave the timestamp assignment outside the reseed branch -- from the real
    /// one, where the other two tests do not (both fill pool 0 before the first call, so both pass
    /// against the naive repair). Measured instead: under the naive repair the boot never reaches
    /// the test harness at all. `getrandom` recursed 33,783 times before a 300s timeout killed the
    /// guest, because the global accumulator's first request happens with pools empty, and every
    /// retry restamps the timestamp back inside the gap.
    ///
    /// So the regression guard for *that* defect is the boot, not this test. What this pins is the
    /// narrower and still-useful thing: a future variant that breaks the same invariant without
    /// also hanging the boot -- which is the only version that would get past a green CI run.
    #[kernel_test]
    fn test_failed_attempt_does_not_consume_the_reseed_gap() {
        let mut acc = Accumulator::new();
        let mut c = Contributor::new();
        let mut out = [0u8; 32];
        // Pools empty, exactly as at boot: this must fail.
        assert!(
            acc.try_fill_random_data(&mut out).is_err(),
            "an accumulator with empty pools must report Unseeded"
        );
        fill_pool_zero(&mut acc, &mut c);
        // Immediately afterwards, mirroring `getrandom`'s recursive retry. Well inside
        // MIN_RESEED_GAP of the failed attempt above, which is the whole point.
        acc.try_fill_random_data(&mut out)
            .expect("retry after entropy must seed: the failed attempt was not a reseed");
        assert!(acc.is_seeded());
    }

    /// A burst of requests must not reseed on every one of them.
    ///
    /// `reseed_ct` is the property here, not a proxy for it: 9.5.5's rule is about how many
    /// reseeds happen, so counting them is the direct test. Mildly timing-dependent -- the burst
    /// has to finish inside `MIN_RESEED_GAP` -- but the margin is three orders of magnitude even
    /// under TCG, and the failure direction is a spurious pass rather than a spurious failure.
    #[kernel_test]
    fn test_burst_does_not_reseed_repeatedly() {
        let mut acc = Accumulator::new();
        let mut c = Contributor::new();
        fill_pool_zero(&mut acc, &mut c);
        let mut out = [0u8; 32];
        acc.try_fill_random_data(&mut out).unwrap();
        assert_eq!(acc.reseed_ct, 1, "first request should reseed exactly once");

        // Refill, so the only thing that can stop a second reseed is the elapsed-time gap and not
        // an empty pool 0. Without this the test would pass even with the gap check deleted.
        fill_pool_zero(&mut acc, &mut c);
        for _ in 0..8 {
            acc.try_fill_random_data(&mut out).unwrap();
        }
        assert_eq!(
            acc.reseed_ct, 1,
            "a burst inside MIN_RESEED_GAP reseeded more than once"
        );
    }
}
