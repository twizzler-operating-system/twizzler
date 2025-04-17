use bitflags::bitflags;

use crate::CapError;

#[derive(PartialEq, Copy, Clone, Debug, Eq, Ord, PartialOrd)]
pub struct CapFlags(u8);

#[rustfmt::skip] // so the bits are all nice and neat
bitflags! {
    impl CapFlags: u8 {
        //NOTE: flags here indicate which algorithm was used for signature generation.
        const Ed25519=  0b00000001;
        const Blake3 =   0b00000010;
        // non removable tag here
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SigningScheme {
    Ed25519,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum HashingAlgo {
    Blake3,
}

impl CapFlags {
    pub(crate) fn parse(&self) -> Result<(HashingAlgo, SigningScheme), CapError> {
        let mut hashing_algo = None;
        let mut signing_scheme = None;

        for flag in self.iter() {
            match flag {
                CapFlags::Ed25519 => {
                    if signing_scheme.is_some() {
                        return Err(CapError::InvalidFlags);
                    }
                    signing_scheme = Some(SigningScheme::Ed25519)
                }
                CapFlags::Blake3 => {
                    if hashing_algo.is_some() {
                        return Err(CapError::InvalidFlags);
                    }
                    hashing_algo = Some(HashingAlgo::Blake3)
                }
                _ => {} // not a fan of this but have to otherwise it bugs you
            };
        }

        // sanity check
        if hashing_algo.is_none() || signing_scheme.is_none() {
            return Err(CapError::InvalidFlags);
        }

        Ok((hashing_algo.unwrap(), signing_scheme.unwrap()))
    }
}
