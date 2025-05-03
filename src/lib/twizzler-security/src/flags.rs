use alloc::string::String;
use core::{fmt::Display, str::FromStr};

use bitflags::bitflags;

use crate::SecError;

#[derive(PartialEq, Copy, Clone, Debug, Eq, Ord, PartialOrd)]
pub struct CapFlags(u16);

#[rustfmt::skip] // so the bits are all nice and neat
bitflags! {
    impl CapFlags: u16 {
        //NOTE: flags here indicate which algorithm was used for signature generation.
        const Ed25519=  1;
        const Blake3 =   2;
        const Sha256 = 4;
        const Ecdsa = 8;
        // non removable tag here
    }
}

impl Display for CapFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CapFlags {");
        for flag in self.iter() {
            match flag {
                CapFlags::Ed25519 => f.write_str(" ED25519 ")?,
                CapFlags::Ecdsa => f.write_str(" Ecdsa ")?,
                CapFlags::Blake3 => f.write_str(" Blake3 ")?,
                CapFlags::Sha256 => f.write_str(" SHA256 ")?,
                // have to do this due to how bitflags work
                _ => (),
            };
        }

        f.write_str("}")?;

        Ok(())
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum SigningScheme {
    Ed25519,
    #[default]
    Ecdsa,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum HashingAlgo {
    #[default]
    Blake3,
    Sha256,
}

impl CapFlags {
    pub(crate) fn parse(&self) -> Result<(HashingAlgo, SigningScheme), SecError> {
        let hashing_algo: HashingAlgo = self.clone().try_into()?;
        let signing_scheme: SigningScheme = self.clone().try_into()?;

        Ok((hashing_algo, signing_scheme))
    }
}

impl TryFrom<CapFlags> for HashingAlgo {
    type Error = SecError;
    fn try_from(value: CapFlags) -> Result<Self, Self::Error> {
        let mut result = None;

        for flag in value.iter() {
            if let Some(algo) = match flag {
                CapFlags::Sha256 => Some(HashingAlgo::Sha256),
                CapFlags::Blake3 => Some(HashingAlgo::Blake3),
                _ => None,
            } {
                if result.is_some() {
                    return Err(SecError::InvalidFlags);
                }

                result = Some(algo);
            }
        }

        result.ok_or(SecError::InvalidFlags)
    }
}
impl TryFrom<CapFlags> for SigningScheme {
    type Error = SecError;
    fn try_from(value: CapFlags) -> Result<Self, Self::Error> {
        let mut result = None;

        for flag in value.iter() {
            if let Some(algo) = match flag {
                CapFlags::Ed25519 => Some(SigningScheme::Ed25519),
                CapFlags::Ecdsa => Some(SigningScheme::Ecdsa),
                _ => None,
            } {
                if result.is_some() {
                    return Err(SecError::InvalidFlags);
                }

                result = Some(algo);
            }
        }

        result.ok_or(SecError::InvalidFlags)
    }
}
