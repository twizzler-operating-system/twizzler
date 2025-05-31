use core::fmt::{Debug, Display};

use bitflags::bitflags;

use crate::SecurityError;

#[derive(PartialEq, Copy, Clone, Eq, Ord, PartialOrd)]
pub struct CapFlags(u16);

#[rustfmt::skip] // so the bits are all nice and neat
bitflags! {
    impl CapFlags: u16 {
        //NOTE: flags here indicate which algorithm was used for hashing
        const Blake3 =   1;
        const Sha256 = 2;
    }
}

impl Display for CapFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CapFlags {")?;
        for flag in self.iter() {
            match flag {
                CapFlags::Blake3 => f.write_str(" Blake3 ")?,
                CapFlags::Sha256 => f.write_str(" SHA256 ")?,
                _ => (),
            };
        }

        f.write_str("}")?;

        Ok(())
    }
}

impl Debug for CapFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum SigningScheme {
    #[default]
    Ecdsa,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum HashingAlgo {
    #[default]
    Blake3,
    Sha256,
}

impl TryFrom<CapFlags> for HashingAlgo {
    type Error = SecurityError;
    fn try_from(value: CapFlags) -> Result<Self, Self::Error> {
        let mut result = None;

        for flag in value.iter() {
            if let Some(algo) = match flag {
                CapFlags::Sha256 => Some(HashingAlgo::Sha256),
                CapFlags::Blake3 => Some(HashingAlgo::Blake3),
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

impl From<HashingAlgo> for CapFlags {
    fn from(value: HashingAlgo) -> Self {
        match value {
            HashingAlgo::Blake3 => CapFlags::Blake3,
            HashingAlgo::Sha256 => CapFlags::Sha256,
        }
    }
}
