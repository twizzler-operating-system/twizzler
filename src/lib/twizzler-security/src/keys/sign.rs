#[cfg(feature = "log")]
use log::{debug, error};
// use ed25519_dalek::{
//     ed25519::signature::SignerMut, SecretKey, SigningKey as EdSigningKey, SECRET_KEY_LENGTH,
//     SIGNATURE_LENGTH,
// };
use p256::ecdsa::{signature::Signer, Signature as EcdsaSignature, SigningKey as EcdsaSigningKey};

use super::{Signature, VerifyingKey, MAX_KEY_SIZE};
use crate::{SecurityError, SigningScheme};

/// The Objects signing key stored internally in the kernel used during the signing of capabilities.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SigningKey {
    key: [u8; MAX_KEY_SIZE],
    len: usize,
    pub scheme: SigningScheme,
}

impl SigningKey {
    pub fn new(scheme: &SigningScheme) -> (Self, VerifyingKey) {
        #[cfg(feature = "log")]
        debug!("Creating new signing key with scheme: {:?}", scheme);

        todo!("do something :sob:")
    }

    /// Builds up a signing key from a slice of bytes and a specified signing scheme.
    pub fn from_slice(slice: &[u8], scheme: SigningScheme) -> Result<Self, SecurityError> {
        match scheme {
            SigningScheme::Ed25519 => {
                unimplemented!("until we figure out whats wrong with data layout")
                // if slice.len() != SECRET_KEY_LENGTH {
                //     return Err(SecurityError::InvalidSigningKey);
                // }

                // let mut buf = [0_u8; MAX_KEY_SIZE];

                // buf[0..SECRET_KEY_LENGTH].copy_from_slice(slice);
                // Ok(Self {
                //     key: buf,
                //     len: slice.len(),
                //     scheme: SigningScheme::Ed25519,
                // })
            }
            SigningScheme::Ecdsa => {
                // the crate doesnt expose a const to verify key length,
                // next best thing is to just ensure that key creation works
                // instead of hardcoding in a key length?
                let key = EcdsaSigningKey::from_slice(slice).map_err(|e| {
                    #[cfg(feature = "log")]
                    error!(
                        "Unable to create EcdsaSigningKey from slice due to: {:#?}!",
                        e
                    );
                    SecurityError::InvalidSigningKey
                })?;

                let binding = key.to_bytes();
                let bytes = &binding.as_slice();

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

    pub fn sign(&self, msg: &[u8]) -> Result<Signature, SecurityError> {
        match self.scheme {
            SigningScheme::Ed25519 => {
                // let mut signing_key: EdSigningKey = self.try_into()?;
                // Ok(signing_key.sign(msg).into())
                unimplemented!("until we figure out whats wrong with data layout")
            }
            SigningScheme::Ecdsa => {
                let mut signing_key: EcdsaSigningKey = self.try_into()?;
                let sig: EcdsaSignature = signing_key.sign(msg);
                Ok(sig.into())
            }
        }
    }
}

// impl TryFrom<&SigningKey> for EdSigningKey {
//     type Error = SecurityError;

//     fn try_from(value: &SigningKey) -> Result<Self, Self::Error> {
//         if value.scheme != SigningScheme::Ed25519 {
//             return Err(SecurityError::InvalidScheme);
//         }

//         let mut buf = [0_u8; SECRET_KEY_LENGTH];
//         buf.copy_from_slice(value.as_bytes());

//         Ok(EdSigningKey::from_bytes(&buf))
//     }
// }

impl TryFrom<&SigningKey> for EcdsaSigningKey {
    type Error = SecurityError;
    fn try_from(value: &SigningKey) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ecdsa {
            #[cfg(feature = "log")]
            error!("Cannot convert SigningKey to EcdsaSigningKey due to scheme mismatch. SigningKey scheme: {:?}", value.scheme);
            return Err(SecurityError::InvalidScheme);
        }

        Ok(EcdsaSigningKey::from_slice(value.as_bytes()).map_err(|e| {
            #[cfg(feature = "log")]
            error!("Cannot build EcdsaSigningKey from slice due to: {:?}", e);
            SecurityError::InvalidSigningKey
        })?)
    }
}
