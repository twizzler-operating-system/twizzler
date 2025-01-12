use serde::{Deserialize, Serialize};
use serde_with::{serde_as, Bytes};

use crate::{
    hasher::{
        blake3::{Blake3, BLAKE3_MD_SIZE},
        Hasher,
    },
    key::Key,
};

use super::KeyDerivationFunction;

#[serde_as]
#[derive(Serialize, Deserialize)]
pub struct Blake3KDF {
    #[serde_as(as = "Bytes")]
    inner: Key<BLAKE3_MD_SIZE>,
}

impl Blake3KDF {
    pub fn new(key: Key<BLAKE3_MD_SIZE>) -> Self {
        Self { inner: key }
    }
}

impl KeyDerivationFunction<BLAKE3_MD_SIZE> for Blake3KDF {
    type KeyId = u64;

    fn with_key(key: Key<BLAKE3_MD_SIZE>) -> Self {
        Self::new(key)
    }

    fn derive(&mut self, key_id: Self::KeyId) -> Key<BLAKE3_MD_SIZE> {
        let mut hasher = Blake3::new();
        hasher.update(&self.inner);
        hasher.update(&key_id.to_le_bytes());
        hasher.finish()
    }
}

#[serde_as]
#[derive(Serialize, Deserialize)]
pub struct LocalizedBlake3KDF {
    #[serde_as(as = "Bytes")]
    inner: Key<BLAKE3_MD_SIZE>,
}

impl LocalizedBlake3KDF {
    pub fn new(key: Key<BLAKE3_MD_SIZE>) -> Self {
        Self { inner: key }
    }
}

impl KeyDerivationFunction<BLAKE3_MD_SIZE> for LocalizedBlake3KDF {
    type KeyId = (u64, u64);

    fn with_key(key: Key<BLAKE3_MD_SIZE>) -> Self {
        Self::new(key)
    }

    fn derive(&mut self, (obj_id, block): Self::KeyId) -> Key<BLAKE3_MD_SIZE> {
        let mut hasher = Blake3::new();
        hasher.update(&self.inner);
        hasher.update(&obj_id.to_le_bytes());
        hasher.update(&block.to_le_bytes());
        hasher.finish()
    }
}
