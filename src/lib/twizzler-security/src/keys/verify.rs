#[cfg(feature = "log")]
use log::{debug, error};
use p256::{
    ecdsa::{
        signature::Verifier, Signature as EcdsaSignature, SigningKey as EcdsaSigningKey,
        VerifyingKey as EcdsaVerifyingKey,
    },
    elliptic_curve::sec1::EncodedPoint,
    NistP256,
};
// use ed25519_dalek::{
//     ed25519, Signature as EdSignature, SigningKey as EdSigningKey, Verifier,
//     VerifyingKey as EdVerifyingKey, PUBLIC_KEY_LENGTH,
// };
#[cfg(feature = "user")]
use twizzler::marker::BaseType;

use super::{Signature, SigningKey, MAX_KEY_SIZE};
use crate::{SecurityError, SigningScheme};

// making our own struct for verifying key since we need to be able to support keys with different
// schemes, (meaning they could also be different lengths)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VerifyingKey {
    key: [u8; MAX_KEY_SIZE],
    len: usize,
    pub scheme: SigningScheme,
}

impl VerifyingKey {
    pub fn new(
        scheme: &SigningScheme,
        target_private_key: &SigningKey,
    ) -> Result<Self, SecurityError> {
        #[cfg(feature = "log")]
        debug!("Creating new verifying key with scheme: {:?}", scheme);
        match scheme {
            SigningScheme::Ed25519 => {
                // let signing_key: EdSigningKey = target_private_key.try_into()?;
                // let vkey = signing_key.verifying_key();
                // let mut buf = [0; MAX_KEY_SIZE];
                // buf[0..PUBLIC_KEY_LENGTH].copy_from_slice(vkey.as_bytes());
                // Ok(VerifyingKey {
                //     key: buf,
                //     len: PUBLIC_KEY_LENGTH,
                //     scheme: *scheme,
                // })
                unimplemented!("until we figure out data layout issue")
            }
            SigningScheme::Ecdsa => {
                let vkey = EcdsaVerifyingKey::from(TryInto::<EcdsaSigningKey>::try_into(
                    target_private_key,
                )?);

                let point = vkey.to_encoded_point(false);
                let bytes = point.as_bytes();

                let mut buf = [0; MAX_KEY_SIZE];
                buf[0..bytes.len()].copy_from_slice(bytes);

                Ok(VerifyingKey {
                    key: buf,
                    len: bytes.len(),
                    scheme: SigningScheme::Ecdsa,
                })
            }
        }
    }

    pub fn from_slice(slice: &[u8], scheme: &SigningScheme) -> Result<Self, SecurityError> {
        match scheme {
            SigningScheme::Ed25519 => {
                // if slice.len() != PUBLIC_KEY_LENGTH {
                //     return Err(SecurityError::InvalidKey);
                // }

                // let mut buf = [0_u8; MAX_KEY_SIZE];

                // buf[0..PUBLIC_KEY_LENGTH].copy_from_slice(slice);
                // Ok(Self {
                //     key: buf,
                //     len: slice.len(),
                //     scheme: SigningScheme::Ed25519,
                // })
                unimplemented!("until we figure out data layout")
            }
            SigningScheme::Ecdsa => {
                let point: EncodedPoint<NistP256> = EncodedPoint::<NistP256>::from_bytes(slice)
                    .map_err(|_e| {
                        #[cfg(feature = "log")]
                        error!(
                            "Unable to create an encoded point from bytes due to :{:?}",
                            _e
                        );

                        SecurityError::InvalidKey
                    })?;

                // we create key here to ensure its valid
                let _key = EcdsaVerifyingKey::from_encoded_point(&point).map_err(|_e| {
                    #[cfg(feature = "log")]
                    error!(
                        "Unable to create an EcdsaVerifyingKey from encoded point, due to :{:?}",
                        _e
                    );

                    SecurityError::InvalidKey
                })?;

                let mut buf = [0; MAX_KEY_SIZE];
                buf[0..slice.len()].copy_from_slice(slice);
                Ok(VerifyingKey {
                    key: buf,
                    len: slice.len(),
                    scheme: SigningScheme::Ecdsa,
                })
            }
        }
    }

    // so we can easily extract out the key without worrying about len and the buffer
    pub fn as_bytes(&self) -> &[u8] {
        &self.key[0..self.len]
    }

    /// Checks whether the `sig` can be verified.
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<(), SecurityError> {
        match self.scheme {
            SigningScheme::Ed25519 => {
                // let vkey: EdVerifyingKey =
                //     self.try_into().map_err(|_| SecurityError::InvalidKey)?;
                // vkey.verify(
                //     msg,
                //     &EdSignature::try_from(sig).map_err(|e| SecurityError::InvalidSignature)?,
                // )
                // .map_err(|_| SecurityError::InvalidSignature)
                unimplemented!("until we figure out data layout")
            }
            SigningScheme::Ecdsa => {
                let key: EcdsaVerifyingKey = self.try_into()?;
                let ecdsa_sig: EcdsaSignature = sig.try_into()?;
                key.verify(msg, &ecdsa_sig).map_err(|_e| {
                    #[cfg(feature = "log")]
                    error!("Failed verification of signature due to: {:#?}", _e);

                    SecurityError::SignatureMismatch
                })
            }
        }
    }
}

// impl TryFrom<&VerifyingKey> for EdVerifyingKey {
//     type Error = SecurityError;

//     fn try_from(value: &VerifyingKey) -> Result<EdVerifyingKey, SecurityError> {
//         if value.scheme != SigningScheme::Ed25519 {
//             return Err(SecurityError::InvalidScheme);
//         }

//         let mut buf = [0_u8; PUBLIC_KEY_LENGTH];
//         buf.copy_from_slice(value.as_bytes());

//         EdVerifyingKey::from_bytes(&buf).map_err(|e| SecurityError::InvalidKey)
//     }
// }
//
impl TryFrom<&VerifyingKey> for EcdsaVerifyingKey {
    type Error = SecurityError;
    fn try_from(value: &VerifyingKey) -> Result<Self, Self::Error> {
        let point: EncodedPoint<NistP256> = EncodedPoint::<NistP256>::from_bytes(value.as_bytes())
            .map_err(|_e| {
                #[cfg(feature = "log")]
                error!(
                    "Failed to create an encoded point from bytes due to :{:#?}",
                    _e
                );

                SecurityError::InvalidKey
            })?;

        let key = EcdsaVerifyingKey::from_encoded_point(&point).map_err(|_e| {
            #[cfg(feature = "log")]
            error!(
                "Failed to create a EcdsaVerifyingKey out of an encoded point due to :{:#?}",
                _e
            );

            SecurityError::InvalidKey
        })?;

        Ok(key)
    }
}

impl From<EcdsaVerifyingKey> for VerifyingKey {
    fn from(value: EcdsaVerifyingKey) -> Self {
        let point = value.to_encoded_point(false);

        let bytes = point.as_bytes();

        let mut buf = [0; MAX_KEY_SIZE];

        buf[0..bytes.len()].copy_from_slice(bytes);

        VerifyingKey {
            key: buf,
            len: bytes.len(),
            scheme: SigningScheme::Ecdsa,
        }
    }
}
#[cfg(feature = "user")]
impl BaseType for VerifyingKey {
    fn fingerprint() -> u64 {
        return 6;
    }
}
