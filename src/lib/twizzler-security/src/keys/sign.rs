#[cfg(feature = "log")]
use log::{debug, error};
// use ed25519_dalek::{
//     ed25519::signature::SignerMut, SecretKey, SigningKey as EdSigningKey, SECRET_KEY_LENGTH,
//     SIGNATURE_LENGTH,
// };
use p256::ecdsa::{signature::Signer, Signature as EcdsaSignature, SigningKey as EcdsaSigningKey};
#[cfg(feature = "user")]
use twizzler::{
    marker::BaseType,
    object::{Object, ObjectBuilder},
};
use twizzler_rt_abi::error::TwzError;
// 256 / 8 => 32 bytes for secret key length, since we are using curve p256, 256 bit curve
const ECDSA_SECRET_KEY_LENGTH: usize = 32;

use twizzler_abi::syscall::ObjectCreate;

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
    #[cfg(feature = "user")]
    /// Creates a new SigningKey / VerifyingKey object pairs.
    pub fn new_keypair(
        scheme: &SigningScheme,
        obj_create_spec: ObjectCreate,
    ) -> Result<(Object<Self>, Object<VerifyingKey>), TwzError> {
        use alloc::borrow::ToOwned;

        use getrandom::getrandom;

        #[cfg(feature = "log")]
        debug!("Creating new signing key with scheme: {:?}", scheme);

        // first create the key using the signing scheme
        let (signing_key, verifying_key): (SigningKey, VerifyingKey) = match scheme {
            SigningScheme::Ed25519 => {
                unimplemented!("still need to fix creating ed25519 keys")
            }

            SigningScheme::Ecdsa => {
                let mut rand_buf = [0_u8; ECDSA_SECRET_KEY_LENGTH];

                if let Err(e) = getrandom(&mut rand_buf) {
                    #[cfg(feature = "log")]
                    error!(
                        "Failed to initialize buffer with random bytes, terminating
                        key creation. Underlying error: {}",
                        e
                    );

                    return Err(TwzError::Generic(
                        twizzler_rt_abi::error::GenericError::Internal,
                    ));
                }

                let Ok(ecdsa_signing_key) = EcdsaSigningKey::from_slice(&rand_buf) else {
                    #[cfg(feature = "log")]
                    error!("Failed to create ecdsa signing key from bytes");

                    return Err(TwzError::Generic(
                        twizzler_rt_abi::error::GenericError::Internal,
                    ));
                };

                let binding = ecdsa_signing_key.clone();

                let ecdsa_verifying_key = binding.verifying_key().to_owned();

                (ecdsa_signing_key.into(), ecdsa_verifying_key.into())
            }
        };

        let s_object = ObjectBuilder::new(obj_create_spec.clone()).build(signing_key)?;
        let v_object = ObjectBuilder::new(obj_create_spec).build(verifying_key)?;

        return Ok((s_object, v_object));
    }

    /// Builds up a signing key from a slice of bytes and a specified signing scheme.
    pub fn from_slice(slice: &[u8], scheme: SigningScheme) -> Result<Self, SecurityError> {
        match scheme {
            SigningScheme::Ed25519 => {
                unimplemented!("until we figure out whats wrong with data layout")
                // if slice.len() != SECRET_KEY_LENGTH {
                //     return Err(SecurityError::InvalidKey);
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
                let key = EcdsaSigningKey::from_slice(slice).map_err(|_e| {
                    #[cfg(feature = "log")]
                    error!(
                        "Unable to create EcdsaSigningKey from slice due to: {:#?}!",
                        _e
                    );
                    SecurityError::InvalidKey
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
                let signing_key: EcdsaSigningKey = self.try_into()?;
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

        Ok(EcdsaSigningKey::from_slice(value.as_bytes()).map_err(|_e| {
            #[cfg(feature = "log")]
            error!("Cannot build EcdsaSigningKey from slice due to: {:?}", _e);
            SecurityError::InvalidKey
        })?)
    }
}

impl From<EcdsaSigningKey> for SigningKey {
    fn from(value: EcdsaSigningKey) -> Self {
        let binding = value.to_bytes();
        let slice = binding.as_slice();

        let mut buf = [0; MAX_KEY_SIZE];

        buf[0..slice.len()].copy_from_slice(slice);

        SigningKey {
            key: buf,
            len: slice.len(),
            scheme: SigningScheme::Ecdsa,
        }
    }
}

mod tests {
    use super::*;

    #[cfg(feature = "user")]
    extern crate test;

    #[cfg(feature = "user")]
    use test::Bencher;

    #[test]
    #[cfg(feature = "user")]
    fn test_key_creation() {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );
        let (skey, vkey) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("keys should be generated properly");
    }

    #[test]
    #[cfg(feature = "user")]
    fn test_signing_and_verification() {
        use twizzler::object::TypedObject;

        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );

        let (s_obj, v_obj) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("Keys should be generated properly");
        let message = "deadbeef".as_bytes();

        let sig = s_obj
            .base()
            .sign(message)
            .expect("Signature should succeed");

        v_obj
            .base()
            .verify(message, &sig)
            .expect("Should be verified properly");
    }

    #[bench]
    //NOTE: currently we can only bench in user space, need to benchmark this in kernel space as
    // well
    #[cfg(feature = "user")]
    fn bench_keypair_creation(b: &mut Bencher) {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            twizzler_abi::syscall::LifetimeType::Persistent,
            Default::default(),
            Default::default(),
        );
        b.iter(|| {
            let (skey, vkey) =
                SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec.clone())
                    .expect("Keys should be generated properly.");
        });
    }
}

#[cfg(feature = "user")]
impl BaseType for SigningKey {
    fn fingerprint() -> u64 {
        return 6;
    }
}
