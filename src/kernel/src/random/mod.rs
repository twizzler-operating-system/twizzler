pub mod cpu_trng;
mod fortuna;
use alloc::vec::Vec;

use rand_core::RngCore;

// 256 bit/32 byte buffer, based on linux's and FreeBSD's key sizes
const BUFFER_SIZE: usize = 8;
struct EntropyBuffer {
    buffer: [u8; BUFFER_SIZE],
}

pub trait EntropySource {
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error>;
}

impl RngCore for dyn EntropySource {
    /// Do not use as Rndrs is fallable. This can panic!
    fn next_u32(&mut self) -> u32 {
        rand_core::impls::next_u32_via_fill(self)
    }
    /// Do not use as Rndrs is fallable. This can panic!
    fn next_u64(&mut self) -> u64 {
        rand_core::impls::next_u64_via_fill(self)
    }
    /// Do not use as Rndrs is fallable. This can panic!
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.try_fill_bytes(dest).unwrap()
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.try_fill_entropy(dest)
    }
}

impl EntropyBuffer {
    pub fn mix_entropy(&mut self, entropy: [u8; BUFFER_SIZE]) {
        for (b, e) in self.buffer.iter_mut().zip(entropy.iter()) {
            *b |= e;
        }
    }
}

pub fn register_entropy_source(source: impl EntropySource) {
    todo!()
}
