use bitflags::bitflags;

use crate::CapError;

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

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SigningScheme {
    Ed25519,
    //TODO: implement this
    Ecdsa,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum HashingAlgo {
    Blake3,
    //TODO: implement this
    Sha256,
}

impl CapFlags {
    pub(crate) fn parse(&self) -> Result<(HashingAlgo, SigningScheme), CapError> {
        let hashing_algo: HashingAlgo = self.try_into()?;
        let signing_scheme: SigningScheme = self.try_into()?;

        Ok((hashing_algo, signing_scheme))
    }
}

impl TryFrom<&CapFlags> for HashingAlgo {
    type Error = CapError;
    fn try_from(value: CapFlags) -> Result<Self, Self::Error> {
        let mut result = None;

        for flag in value.iter() {
            if let Some(algo) = match flag {
                CapFlags::Sha256 => Some(HashingAlgo::Sha256),
                CapFlags::Blake3 => Some(HashingAlgo::Blake3),
                _ => None,
            } {
                if result.is_some() {
                    return Err(CapError::InvalidFlags);
                }

                result = Some(algo);
            }
        }

        result.ok_or(CapError::InvalidFlags)
    }
}
impl TryFrom<&CapFlags> for SigningScheme {
    type Error = CapError;
    fn try_from(value: CapFlags) -> Result<Self, Self::Error> {
        let mut result = None;

        for flag in value.iter() {
            if let Some(algo) = match flag {
                CapFlags::Ed25519 => Some(SigningScheme::Ed25519),
                CapFlags::Ecdsa => Some(SigningScheme::Ecdsa),
                _ => None,
            } {
                if result.is_some() {
                    return Err(CapError::InvalidFlags);
                }

                result = Some(algo);
            }
        }

        result.ok_or(CapError::InvalidFlags)
    }
}
