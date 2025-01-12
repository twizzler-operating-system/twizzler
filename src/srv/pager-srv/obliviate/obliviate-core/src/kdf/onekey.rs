use serde::{Deserialize, Serialize};
use serde_with::{serde_as, Bytes};

use crate::key::Key;

use super::KeyDerivationFunction;

pub const ONEKEY_MD_SIZE: usize = 32;

#[serde_as]
#[derive(Serialize, Deserialize)]
pub struct OneKeyKDF {
    #[serde_as(as = "Bytes")]
    key: Key<ONEKEY_MD_SIZE>,
}

impl OneKeyKDF {
    pub fn new(key: Key<ONEKEY_MD_SIZE>) -> Self {
        Self { key }
    }
}

impl KeyDerivationFunction<ONEKEY_MD_SIZE> for OneKeyKDF {
    type KeyId = u64;

    fn with_key(key: Key<ONEKEY_MD_SIZE>) -> Self {
        Self::new(key)
    }

    fn derive(&mut self, _key_id: Self::KeyId) -> Key<ONEKEY_MD_SIZE> {
        self.key
    }
}
