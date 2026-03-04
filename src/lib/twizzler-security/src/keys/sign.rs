#[cfg(feature = "log")]
use log::{debug, error};
#[cfg(feature = "user")]
use {
    twizzler::{
        marker::BaseType,
        object::{Object, ObjectBuilder},
    },
    twizzler_abi::syscall::ObjectCreate,
};

// 256 / 8 => 32 bytes for secret key length, since we are using curve p256, 256 bit curve
const ECDSA_SECRET_KEY_LENGTH: usize = 32;

use p256::ecdsa::{signature::Signer, Signature as EcdsaSignature, SigningKey as EcdsaSigningKey};
use twizzler_rt_abi::error::TwzError;

#[cfg(feature = "kernel")]
use super::MAX_KEY_SIZE;
use super::{KeyBuffer, Signature, VerifyingKey};
use crate::{SecurityError, SigningScheme};

/// Helper type for keybuffer

/// An Objects `SigningKey`, used in creating `Cap`s and `Del`s.
/// Is agnostic over SigningSchemes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningKey {
    /// The buffer that stores the raw key bytes.
    key: KeyBuffer,
    /// The signing scheme of this key
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

        #[cfg(feature = "user")]
        use getrandom::getrandom;

        #[cfg(feature = "log")]
        debug!("Creating new signing key with scheme: {:?}", scheme);

        // first create the key using the signing scheme
        let (signing_key, verifying_key): (SigningKey, VerifyingKey) = match scheme {
            SigningScheme::Ecdsa => {
                let mut rand_buf = [0_u8; ECDSA_SECRET_KEY_LENGTH];

                if let Err(_e) = getrandom(&mut rand_buf) {
                    #[cfg(feature = "log")]
                    error!(
                        "Failed to initialize buffer with random bytes, terminating
                        key creation. Underlying error: {}",
                        _e
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

    #[cfg(feature = "kernel")]
    /// Creates a new SigningKey / VerifyingKey object pairs.
    pub fn new_kernel_keypair(
        scheme: &SigningScheme,
        random_bytes: [u8; MAX_KEY_SIZE],
    ) -> Result<(SigningKey, VerifyingKey), TwzError> {
        match scheme {
            SigningScheme::Ecdsa => {
                let Ok(ecdsa_signing_key) =
                    EcdsaSigningKey::from_slice(&random_bytes[0..ECDSA_SECRET_KEY_LENGTH])
                else {
                    #[cfg(feature = "log")]
                    error!("Failed to create ecdsa signing key from bytes");

                    return Err(TwzError::Generic(
                        twizzler_rt_abi::error::GenericError::Internal,
                    ));
                };

                let binding = ecdsa_signing_key.clone();

                let ecdsa_verifying_key = binding.verifying_key().clone();

                Ok((ecdsa_signing_key.into(), ecdsa_verifying_key.into()))
            }
        }
    }

    /// Builds up a signing key from a slice of bytes and a specified signing scheme.
    pub fn from_slice(slice: &[u8], scheme: SigningScheme) -> Result<Self, SecurityError> {
        match scheme {
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

                let key = KeyBuffer::from_slice(&binding)
                    .expect("The ECDSA key cannot fit in the currently configured MAX_KEY_SIZE");

                Ok(Self {
                    key,
                    scheme: SigningScheme::Ecdsa,
                })
            }
        }
    }

    /// returns the inner signature as bytes
    pub fn as_bytes(&self) -> &[u8] {
        self.key.as_slice()
    }

    /// Sign the `msg` slice, returning a signature.
    pub fn sign(&self, msg: &[u8]) -> Result<Signature, SecurityError> {
        match self.scheme {
            SigningScheme::Ecdsa => {
                let signing_key: EcdsaSigningKey = self.try_into()?;
                let sig: EcdsaSignature = signing_key.sign(msg);
                Ok(sig.into())
            }
        }
    }
}

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

        let key = KeyBuffer::from_slice(binding.as_slice())
            .expect("The ECDSA key cannot fit in the currently configured MAX_KEY_SIZE");

        SigningKey {
            key,
            scheme: SigningScheme::Ecdsa,
        }
    }
}

#[cfg(feature = "user")]
#[allow(unused_imports)]
mod tests {

    use twizzler_abi::{object::Protections, syscall::ObjectCreate};

    extern crate test;

    use test::Bencher;

    use super::SigningKey;
    use crate::SigningScheme;

    #[test]
    fn test_key_creation() {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Protections::all(),
        );
        let (_skey, _vkey) = SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec)
            .expect("keys should be generated properly");
    }

    #[test]
    fn test_signing_and_verification() {
        use twizzler::object::TypedObject;
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Protections::all(),
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
    fn bench_keypair_creation(b: &mut Bencher) {
        let object_create_spec = ObjectCreate::new(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Protections::all(),
        );
        b.iter(|| {
            let (_skey, _vkey) =
                SigningKey::new_keypair(&SigningScheme::Ecdsa, object_create_spec.clone())
                    .expect("Keys should be generated properly.");
        });
    }
}

#[cfg(feature = "user")]
use twizzler::object::{MapFlags, ObjID, TypedObject};
#[cfg(feature = "user")]
impl TryFrom<ObjID> for SigningKey {
    type Error = TwzError;
    fn try_from(value: ObjID) -> Result<Self, Self::Error> {
        let obj = Object::<SigningKey>::map(value, MapFlags::READ)?;
        Ok(obj.base().clone())
    }
}

#[cfg(feature = "user")]
impl BaseType for SigningKey {
    fn fingerprint() -> u64 {
        return 6;
    }
}
