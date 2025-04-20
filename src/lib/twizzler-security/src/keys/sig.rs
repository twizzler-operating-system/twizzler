use ed25519_dalek::{Signature as EdSignature, SIGNATURE_LENGTH};
use p256::ecdsa::{signature::PrehashSignature, Signature as EcdsaSignature};

use super::MAX_SIG_SIZE;
use crate::{SecError, SigningScheme};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    buf: [u8; MAX_SIG_SIZE],
    pub len: usize,
    scheme: SigningScheme,
}

impl Signature {
    fn as_bytes(&self) -> &[u8] {
        &self.buf[0..self.len]
    }
}

impl From<EdSignature> for Signature {
    fn from(value: EdSignature) -> Self {
        let mut buf = [0_u8; MAX_SIG_SIZE];

        buf[0..SIGNATURE_LENGTH].copy_from_slice(&value.to_bytes());

        Self {
            buf,
            len: SIGNATURE_LENGTH,
            scheme: SigningScheme::Ed25519,
        }
    }
}

impl From<EcdsaSignature> for Signature {
    fn from(value: EcdsaSignature) -> Self {
        let mut buf = [0_u8; MAX_SIG_SIZE];
        let binding = value.to_bytes();
        let slice = binding.as_slice();
        buf[0..slice.len()].copy_from_slice(slice);

        Self {
            buf,
            len: slice.len(),
            scheme: SigningScheme::Ecdsa,
        }
    }
}

impl TryFrom<&Signature> for EdSignature {
    type Error = SecError;
    fn try_from(value: &Signature) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ed25519 {
            return Err(SecError::InvalidScheme);
        }

        Ok(EdSignature::from_slice(value.as_bytes()).map_err(|_| SecError::InvalidSignature)?)
    }
}

impl TryFrom<&Signature> for EcdsaSignature {
    type Error = SecError;
    fn try_from(value: &Signature) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ecdsa {
            return Err(SecError::InvalidScheme);
        }

        Ok(EcdsaSignature::from_slice(value.as_bytes()).map_err(|_| SecError::InvalidSignature)?)
    }
}
