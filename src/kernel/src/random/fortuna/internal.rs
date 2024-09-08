use chacha20::{
    cipher::{KeyIvInit, StreamCipher},
    ChaCha20,
};
use digest::Digest;
use sha2::Sha256;

// based on Cryptography Engineering Chapter 9 by Neils Ferguson et. al.
// comments including 9.x.x reference the above text's sections

const KEY_LENGTH: usize = 32;
const COUNTER_LENGTH: usize = 16;
const CHA_CHA_BLOCK_SIZE: usize = 64;
pub const MAX_GEN_SIZE: usize = 1 << 20;

// 9.4: The internal state of the generator consists of a 256-bit block cipher
// key and a 128-bit counter.
pub struct Generator {
    key: [u8; 32],
    counter: [u8; 16],
}
impl Generator {
    // 9.4.1 We set the key and the counter to zero to indicate that
    // the generator has not been seeded yet.
    /// initializes the generator.
    pub fn new() -> Generator {
        Self {
            key: [0; KEY_LENGTH],
            counter: [0; COUNTER_LENGTH],
        }
    }

    fn increment_counter(&mut self) {
        for i in 0..self.counter.len() {
            let mut overflow = false;
            (self.counter[i], overflow) = self.counter[i].overflowing_add(1);
            if !overflow {
                break;
            }
        }
    }

    // 9.4.2
    pub fn reseed(&mut self, seed: &[u8]) {
        // K <- SHAd-256(K || seed)
        // uses fortuna instead of sha, but it's the same idea,
        // just with a different cipher
        let mut hasher = Sha256::new();
        hasher.update(self.key);
        hasher.update(seed);
        hasher.finalize_into_reset((&mut self.key).into());
        // C <- C+1
        self.increment_counter();
    }
    // 9.4.3 generates blocks into the provided buffer
    // internal function
    fn generate_blocks(&mut self, into: &mut [u8]) {
        debug_assert_ne!(self.counter, [0; COUNTER_LENGTH]);

        debug_assert_eq!(0, into.len() % COUNTER_LENGTH); // assert slice is evenly divisable by COUNTER_LENGTH
        let block_count = into.len() / COUNTER_LENGTH;
        let mut hasher = ChaCha20::new((&self.key).into(), (&self.counter[4..]).into());

        let out_chunks = into.chunks_mut(COUNTER_LENGTH);

        for chunk in out_chunks {
            hasher.apply_keystream_b2b(&self.counter, chunk).unwrap();
            self.increment_counter();
        }
    }
    // 9.4.4 completely fills `out` based on the length of the provided out buffer
    pub fn generate_random_data(&mut self, out: &mut [u8]) {
        assert!(out.len() <= MAX_GEN_SIZE);
        let (n, rem) = (out.len() / COUNTER_LENGTH, out.len() % COUNTER_LENGTH);
        self.generate_blocks(&mut out[..(n * COUNTER_LENGTH)]);
        if rem > 0 {
            let mut buf = [0; COUNTER_LENGTH];
            self.generate_blocks(&mut buf);
            let mut leftover_out = &mut out[(n * COUNTER_LENGTH)..];
            leftover_out.copy_from_slice(&buf);
        }
        let mut new_key = [0; KEY_LENGTH];
        self.generate_blocks(&mut new_key);
    }
}
