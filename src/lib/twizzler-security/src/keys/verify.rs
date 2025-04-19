use ed25519_dalek::{
    ed25519, Signature as EdSignature, SigningKey as EdSigningKey, Verifier,
    VerifyingKey as EdVerifyingKey, PUBLIC_KEY_LENGTH,
};

use super::{KeyError, Signature, SigningKey, MAX_KEY_SIZE};
use crate::{CapError, SigningScheme};

// making our own struct for verifying key since we need to be able to support keys with different
// schemes, (meaning they could also be different lengths)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VerifyingKey {
    key: [u8; MAX_KEY_SIZE],
    len: usize,
    pub scheme: SigningScheme,
}

impl VerifyingKey {
    pub fn new(scheme: SigningScheme, target_private_key: SigningKey) -> Result<Self, CapError> {
        match scheme {
            SigningScheme::Ed25519 => {
                let signing_key: EdSigningKey = (&target_private_key)
                    .try_into()
                    .map_err(|_e| CapError::InvalidPrivateKey)?;
                let vkey = signing_key.verifying_key();
                let mut buff = [0; MAX_KEY_SIZE];
                buff[0..PUBLIC_KEY_LENGTH].copy_from_slice(vkey.as_bytes());
                Ok(VerifyingKey {
                    key: buff,
                    len: PUBLIC_KEY_LENGTH,
                    scheme,
                })
            }
        }
    }

    pub fn from_slice(slice: &[u8], scheme: SigningScheme) -> Result<Self, KeyError> {
        match scheme {
            SigningScheme::Ed25519 => {
                if slice.len() != PUBLIC_KEY_LENGTH {
                    return Err(KeyError::InvalidKeyLength);
                }

                let mut buf = [0_u8; MAX_KEY_SIZE];

                buf[0..PUBLIC_KEY_LENGTH].copy_from_slice(slice);
                Ok(Self {
                    key: buf,
                    len: slice.len(),
                    scheme: SigningScheme::Ed25519,
                })
            }
        }
    }

    // so we can easily extract out the key without worrying about len and the buffer
    pub fn as_bytes(&self) -> &[u8] {
        &self.key[0..self.len]
    }

    /// Checks whether the `sig` can be verified.
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<(), CapError> {
        match self.scheme {
            SigningScheme::Ed25519 => {
                let vkey: EdVerifyingKey =
                    self.try_into().map_err(|_| CapError::InvalidVerifyKey)?;
                vkey.verify(
                    msg,
                    &EdSignature::try_from(sig).map_err(|e| CapError::InvalidSignature)?,
                )
                .map_err(|_| CapError::InvalidSignature)
            }
        }
    }
}

impl TryFrom<&VerifyingKey> for EdVerifyingKey {
    type Error = KeyError;

    fn try_from(value: &VerifyingKey) -> Result<EdVerifyingKey, KeyError> {
        if value.scheme != SigningScheme::Ed25519 {
            return Err(KeyError::InvalidScheme);
        }

        let mut buf = [0_u8; PUBLIC_KEY_LENGTH];
        buf.copy_from_slice(value.as_bytes());

        //TODO: this isnt the right error map, work on the error types and adjust accordingly, for
        // all
        EdVerifyingKey::from_bytes(&buf).map_err(|e| KeyError::InvalidScheme)
    }
}
