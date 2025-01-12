pub mod blake3;
pub mod chacha20;
pub mod onekey;

use crate::key::Key;

/// A trait for key derivation functions.
pub trait KeyDerivationFunction<const N: usize> {
    /// The type of a key ID.
    type KeyId: Copy;

    /// Create a new instance of the function with a given key.
    fn with_key(key: Key<N>) -> Self;

    /// Derives the key with the given key ID.
    fn derive(&mut self, key_id: Self::KeyId) -> Key<N>;
}
