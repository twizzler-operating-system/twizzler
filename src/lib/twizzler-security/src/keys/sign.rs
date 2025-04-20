use ed25519_dalek::{
    ed25519::signature::SignerMut, SecretKey, SigningKey as EdSigningKey, SECRET_KEY_LENGTH,
    SIGNATURE_LENGTH,
};
use p256::ecdsa::SigningKey as EcdsaSigningKey;

use super::{KeyError, Signature, VerifyingKey, MAX_KEY_SIZE};
use crate::{CapError, SigningScheme};

/// The Objects signing key stored internally in the kernel used during the signing of capabilities.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SigningKey {
    key: [u8; MAX_KEY_SIZE],
    len: usize,
    pub scheme: SigningScheme,
}

impl SigningKey {
    //TODO: gate this behind the `kernel` feature
    pub fn new(scheme: SigningScheme) -> (Self, VerifyingKey) {
        let secret_key = SecretKey::default();
        todo!("should generate signing/verifying key in kernel")
    }

    /// Builds up a signing key from a slice of bytes and a specified signing scheme.
    pub fn from_slice(slice: &[u8], scheme: SigningScheme) -> Result<Self, KeyError> {
        match scheme {
            SigningScheme::Ed25519 => {
                if slice.len() != SECRET_KEY_LENGTH {
                    return Err(KeyError::InvalidKeyLength);
                }

                let mut buf = [0_u8; MAX_KEY_SIZE];

                buf[0..SECRET_KEY_LENGTH].copy_from_slice(slice);
                Ok(Self {
                    key: buf,
                    len: slice.len(),
                    scheme: SigningScheme::Ed25519,
                })
            }
            SigningScheme::Ecdsa => {
                // the crate doesnt expose a const to verify key length,
                // next best thing is to just ensure that key creation works
                // instead of hardcoding in a key length?
                let key = EcdsaSigningKey::from_slice(slice).map_err(KeyError::InvalidKeyLength)?;
                let bytes = key.to_bytes().as_slice();

                let mut buf = [0_u8; MAX_KEY_SIZE];

                buf[0..bytes.len()].copy_from_slice(bytes);

                Ok(Self {
                    key: buf,
                    len: bytes.len(),
                    scheme: SigningScheme::Ecdsa,
                })
            }
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.key[0..self.len]
    }

    pub fn sign(&self, msg: &[u8]) -> Result<Signature, KeyError> {
        match self.scheme {
            SigningScheme::Ed25519 => {
                let mut signing_key: EdSigningKey = self.try_into()?;
                Ok(signing_key.sign(msg).into())
            }
            SigningScheme::Ecdsa => {
                let mut signing_key: EcdsaSigningKey = self.try_into()?;
                // let key = EcdsaSigningKey::from_slice(self.as_bytes())
                //     .map_err(KeyError::InvalidKeyLength)?;
                let sig = signing_key.sign(msg);
            }
        }
    }
}

impl TryFrom<&SigningKey> for EdSigningKey {
    type Error = KeyError;

    fn try_from(value: &SigningKey) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ed25519 {
            return Err(KeyError::InvalidScheme);
        }

        Ok(EdSigningKey::from_bytes(value.as_bytes()))
    }
}

impl TryFrom<&SigningKey> for EcdsaSigningKey {
    type Error = KeyError;
    fn try_from(value: &SigningKey) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ecdsa {
            return Err(KeyError::InvalidScheme);
        }

        Ok(EcdsaSigningKey::from_slice(value.as_bytes()))
    }
}
