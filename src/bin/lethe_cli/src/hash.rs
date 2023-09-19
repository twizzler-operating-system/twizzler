
use hasher::Hasher;
use sha3::{Digest, Sha3_256};
use std::convert::TryInto;

pub struct Hashbrowns {
    hasher : Sha3_256
}

impl Hasher<32> for Hashbrowns {
    fn new() -> Self {
        let x = Sha3_256::new();
        
        Hashbrowns { hasher: x }
    }

    fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    fn finish(self) -> [u8; 32] {
        let x = self.hasher.finalize();

        x.as_slice().try_into().expect("Wrong length")
    }

    fn digest(data: &[u8]) -> [u8; 32] {
        let mut x = Sha3_256::new();

        x.update(data);

        let y = x.finalize();

        y.as_slice().try_into().expect("Wrong length")
    }
}