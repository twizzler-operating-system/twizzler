use ed25519_dalek::{
    ed25519, Signature as EdSignature, SigningKey as EdSigningKey, Verifier,
    VerifyingKey as EdVerifyingKey, PUBLIC_KEY_LENGTH,
};
use p256::{
    ecdsa::{SigningKey as EcdsaSigningKey, VerifyingKey as EcdsaVerifyingKey},
    elliptic_curve::sec1::EncodedPoint,
};

use super::{Signature, SigningKey, MAX_KEY_SIZE};
use crate::{SecError, SigningScheme};

// making our own struct for verifying key since we need to be able to support keys with different
// schemes, (meaning they could also be different lengths)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VerifyingKey {
    key: [u8; MAX_KEY_SIZE],
    len: usize,
    pub scheme: SigningScheme,
}

impl VerifyingKey {
    pub fn new(scheme: &SigningScheme, target_private_key: &SigningKey) -> Result<Self, SecError> {
        match scheme {
            SigningScheme::Ed25519 => {
                let signing_key: EdSigningKey = target_private_key.try_into()?;
                let vkey = signing_key.verifying_key();
                let mut buf = [0; MAX_KEY_SIZE];
                buf[0..PUBLIC_KEY_LENGTH].copy_from_slice(vkey.as_bytes());
                Ok(VerifyingKey {
                    key: buf,
                    len: PUBLIC_KEY_LENGTH,
                    scheme: *scheme,
                })
            }
            SigningScheme::Ecdsa => {
                unimplemented!("Workout how ecdsa verifying key is formed")
                // let vkey = EcdsaVerifyingKey::from(TryInto::<EcdsaSigningKey>::try_into(
                //     target_private_key,
                // )?);
                // let sec1 = vkey.to_encoded_point(false);
                // let bytes = vkey.to_sec1_bytes().as_slice();

                // let mut buf = [0; MAX_KEY_SIZE];
                // buf[0..bytes.len()].copy_from_slice(bytes);

                // Ok(VerifyingKey {
                //     key: buf,
                //     len: bytes.len(),
                //     scheme: SigningScheme::Ecdsa,
                // })
            }
        }
    }

    pub fn from_slice(slice: &[u8], scheme: &SigningScheme) -> Result<Self, SecError> {
        match scheme {
            SigningScheme::Ed25519 => {
                if slice.len() != PUBLIC_KEY_LENGTH {
                    return Err(SecError::InvalidVerifyKey);
                }

                let mut buf = [0_u8; MAX_KEY_SIZE];

                buf[0..PUBLIC_KEY_LENGTH].copy_from_slice(slice);
                Ok(Self {
                    key: buf,
                    len: slice.len(),
                    scheme: SigningScheme::Ed25519,
                })
            }
            SigningScheme::Ecdsa => {
                unimplemented!("figure out how to deserialize this key")
                // let point =
                //     EncodedPoint::from_bytes(slice).map_err(|_| SecError::InvalidVerifyKey)?;

                // let key = EcdsaVerifyingKey::from_encoded_point(point)
                //     .map_err(|_| SecError::InvalidVerifyKey)?;

                // // ok its valid!
                // let mut buf = [0; MAX_KEY_SIZE];
                // buf[0..slice.len()].copy_from_slice(slice);
                // Ok(VerifyingKey {
                //     key: buf,
                //     len: slice.len(),
                //     scheme: SigningScheme::Ecdsa,
                // })
            }
        }
    }

    // so we can easily extract out the key without worrying about len and the buffer
    pub fn as_bytes(&self) -> &[u8] {
        &self.key[0..self.len]
    }

    /// Checks whether the `sig` can be verified.
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<(), SecError> {
        match self.scheme {
            SigningScheme::Ed25519 => {
                let vkey: EdVerifyingKey =
                    self.try_into().map_err(|_| SecError::InvalidVerifyKey)?;
                vkey.verify(
                    msg,
                    &EdSignature::try_from(sig).map_err(|e| SecError::InvalidSignature)?,
                )
                .map_err(|_| SecError::InvalidSignature)
            }
            SigningScheme::Ecdsa => {
                unimplemented!("Workout how ecdsa signature is formed")
            }
        }
    }
}

impl TryFrom<&VerifyingKey> for EdVerifyingKey {
    type Error = SecError;

    fn try_from(value: &VerifyingKey) -> Result<EdVerifyingKey, SecError> {
        if value.scheme != SigningScheme::Ed25519 {
            return Err(SecError::InvalidScheme);
        }

        let mut buf = [0_u8; PUBLIC_KEY_LENGTH];
        buf.copy_from_slice(value.as_bytes());

        EdVerifyingKey::from_bytes(&buf).map_err(|e| SecError::InvalidVerifyKey)
    }
}
