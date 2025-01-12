use std::{fmt, ops::Deref};

use lru_mem::HeapSize;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, Bytes};

/// A key is represented as a contiguous array of bytes.
pub type Key<const N: usize> = [u8; N];

/// A wrapper around a key for serialization and debug printing.
#[serde_as]
#[derive(Deserialize, Serialize, Clone, Copy, HeapSize)]
pub struct KeyWrapper<const N: usize>(#[serde_as(as = "Bytes")] pub Key<N>);

impl<const N: usize> Deref for KeyWrapper<N> {
    type Target = Key<N>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const N: usize> fmt::Debug for KeyWrapper<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

/// An extension trait for generating cryptographic keys.
pub trait KeyGenerator<const N: usize>: RngCore + CryptoRng {
    /// Generates a new key.
    fn gen_key(&mut self) -> Key<N> {
        let mut key = [0; N];
        self.fill_bytes(&mut key);
        key
    }
}

/// All CSPRNGs are automatically key generators.
impl<R, const N: usize> KeyGenerator<N> for R where R: RngCore + CryptoRng {}
