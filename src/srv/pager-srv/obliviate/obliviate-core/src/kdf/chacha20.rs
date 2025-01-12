use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, Bytes};

use crate::key::{Key, KeyGenerator};

use super::KeyDerivationFunction;

pub const CHACHA20KDF_MD_SIZE: usize = 32;

#[serde_as]
#[derive(Serialize, Deserialize)]
pub struct ChaCha20KDF {
    #[serde_as(as = "Bytes")]
    buf: Key<CHACHA20KDF_MD_SIZE>,
}

impl ChaCha20KDF {
    pub fn new(key: Key<CHACHA20KDF_MD_SIZE>) -> Self {
        Self { buf: key }
    }
}

impl KeyDerivationFunction<CHACHA20KDF_MD_SIZE> for ChaCha20KDF {
    type KeyId = u64;

    fn with_key(key: Key<CHACHA20KDF_MD_SIZE>) -> Self {
        Self::new(key)
    }

    fn derive(&mut self, key_id: Self::KeyId) -> Key<CHACHA20KDF_MD_SIZE> {
        self.buf[..std::mem::size_of::<u64>()].copy_from_slice(&key_id.to_le_bytes());
        let mut rng = ChaCha20Rng::from_seed(self.buf);
        rng.gen_key()
    }
}
