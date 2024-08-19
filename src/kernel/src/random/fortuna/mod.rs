mod error;
mod internal;
mod pool;
use alloc::vec::Vec;
use core::{borrow::BorrowMut, time::Duration};

use digest::Digest;
use error::{
    Error,
    TooBigOrSmall::{TooBig, TooSmall},
};
use internal::{Generator, MAX_GEN_SIZE};
use pool::Pool;
use sha2::Sha256;
use twizzler_abi::syscall::Clock;

use crate::{mutex::Mutex, once::Once};

// 9.5.5
const MIN_POOL_SIZE: usize = 64;

// based on Cryptography Engineering Chapter 9 by Neils Ferguson et. al.
// comments including 9.x.x reference the above text's sections

const POOL_COUNT: usize = 32;

const CONTRIBUTOR_ID: Once<Mutex<u8>> = Once::new();
// 9.5.6 utility class to make it easier to keep track of
// incrementing the pool number and make assigning ids easier as well.
pub struct Contributor {
    id: u8,
    pool_number: u8,
}

impl Contributor {
    pub fn new() -> Self {
        let mut contrib_id = CONTRIBUTOR_ID.call_once(|| Mutex::new(0)).lock();
        let out = Contributor {
            id: contrib_id,
            pool_number: 0,
        };
        *contrib_id += 1;
        out
    }
    pub(self) fn contribute(&mut self) -> (u8, u8) {
        self.pool_number += 1;
        (self.id, self.pool_number - 1)
    }
}

// 9.5.4
pub struct Accumulator {
    generator: Generator,
    reseed_ct: usize,
    pools: [Pool; POOL_COUNT],
    last_reseed_timestamp: Duration,
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
            last_reseed_timestamp: Duration::new(0, 0),
        }
    }

    // 9.5.5
    pub fn try_fill_random_data(&mut self, out: &mut [u8]) -> Result<(), self::error::Error> {
        // TODO: require that reseeds are only done at most every 100ms
        if self.pools[0].count() >= MIN_POOL_SIZE {
            self.reseed_ct += 1;
            let mut new_seed: Sha256 = Sha256::new();
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
    pub fn add_random_event(
        &mut self,
        contributor: &mut Contributor,
        data: &[u8],
    ) -> Result<(), Error> {
        let (source_number, pool_number) = contributor.contribute();
        if data.len() < 1 {
            return Err(Error::InvalidDataSize(TooSmall));
        }
        if data.len() > 32 {
            return Err(Error::InvalidDataSize(TooBig));
        }
        if pool_number > POOL_COUNT - 1 {
            return Err(Error::InvalidPoolSize(TooBig));
        }
        self.pools[pool_number].insert(&[source_number, e.len() as u8]);
        self.pools[pool_number].insert(e);
        Ok(())
    }
}
