use ed25519_dalek::{Signature as EdSignature, SIGNATURE_LENGTH};
use p256::ecdsa::Signature as EcdsaSignature;

use super::{KeyError, MAX_SIG_SIZE};
use crate::{CapError, SigningScheme};

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

impl TryFrom<&Signature> for EdSignature {
    type Error = KeyError;
    fn try_from(value: &Signature) -> Result<Self, KeyError> {
        if value.scheme != SigningScheme::Ed25519 {
            return Err(KeyError::InvalidScheme);
        }

        Ok(EdSignature::from_bytes(value.as_bytes()))
    }
}

impl TryFrom<&Signature> for EcdsaSignature {
    type Error = KeyError;
    fn try_from(value: &Signature) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ecdsa {
            return Err(KeyError::InvalidScheme);
        }

        Ok(EcdsaSignature::from_slice(value.as_bytes()))
    }
}
