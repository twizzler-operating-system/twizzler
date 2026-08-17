use core::fmt::{Debug, Display};

use bitflags::bitflags;
use sha2::{Digest, Sha256};

use crate::SecurityError;

#[derive(PartialEq, Copy, Clone, Eq, Ord, PartialOrd)]
/// Flags pertaining to a `Cap`
/// Currently only used to set which hashing scheme to
/// use when forming a capability.
pub struct SecFlags(u16);

bitflags! {
    impl SecFlags: u16 {
        /// The Blake3 Hashing Algorithm was used
        const Blake3 = 1;
        /// The SHA256 Hashing Algorithm was used
        const Sha256 = 2;
    }
}

impl Display for SecFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CapFlags {")?;
        for flag in self.iter() {
            match flag {
                SecFlags::Blake3 => f.write_str(" Blake3 ")?,
                SecFlags::Sha256 => f.write_str(" SHA256 ")?,
                _ => (),
            };
        }

        f.write_str("}")?;

        Ok(())
    }
}

impl Debug for SecFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CapFlags")
            .field("blake3", &self.contains(SecFlags::Blake3))
            .field("sha256", &self.contains(SecFlags::Sha256))
            .field("bits", &self.bits())
            .finish()
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
/// The signing scheme used when creating a `Cap`
pub enum SigningScheme {
    #[default]
    /// Use the ECDSA signing scheme
    Ecdsa,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
/// The hashing algorithm used when creating a `Cap`
pub enum HashingAlgo {
    #[default]
    /// Use the Blake3 Hashing Algorithm
    Blake3,
    /// Use the SHA256 Hashing Algorithm
    Sha256,
}

impl TryFrom<SecFlags> for HashingAlgo {
    type Error = SecurityError;
    fn try_from(value: SecFlags) -> Result<Self, Self::Error> {
        let mut result = None;

        for flag in value.iter() {
            if let Some(algo) = match flag {
                SecFlags::Sha256 => Some(HashingAlgo::Sha256),
                SecFlags::Blake3 => Some(HashingAlgo::Blake3),
                _ => None,
            } {
                if result.is_some() {
                    return Err(SecurityError::InvalidScheme);
                }

                result = Some(algo);
            }
        }

        result.ok_or(SecurityError::InvalidScheme)
    }
}

impl From<HashingAlgo> for SecFlags {
    fn from(value: HashingAlgo) -> Self {
        match value {
            HashingAlgo::Blake3 => SecFlags::Blake3,
            HashingAlgo::Sha256 => SecFlags::Sha256,
        }
    }
}

impl HashingAlgo {
    /// Hashes `data` with the chosen algorithm.
    pub(crate) fn hash(self, data: &[u8]) -> sha2::digest::Output<Sha256> {
        match self {
            HashingAlgo::Blake3 => blake3::Hasher::digest(data),
            HashingAlgo::Sha256 => Sha256::digest(data),
        }
    }
}
