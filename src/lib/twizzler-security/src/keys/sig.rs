use core::fmt::Display;

#[cfg(feature = "log")]
use log::error;
use p256::ecdsa::Signature as EcdsaSignature;

use crate::{SecurityError, SigningScheme};

const MAX_SIG_SIZE: usize = 128;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    //TODO: could just replace this as a heapless vec
    buf: [u8; MAX_SIG_SIZE],
    pub len: usize,
    scheme: SigningScheme,
}

impl Signature {
    fn as_bytes(&self) -> &[u8] {
        &self.buf[0..self.len]
    }
}

impl Display for Signature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Signature(scheme: {:?}, len: {}, bytes: ",
            self.scheme, self.len
        )?;
        for byte in &self.buf[0..self.len] {
            write!(f, "{:02x}", byte)?;
        }
        write!(f, ")")
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

impl TryFrom<&Signature> for EcdsaSignature {
    type Error = SecurityError;
    fn try_from(value: &Signature) -> Result<Self, Self::Error> {
        if value.scheme != SigningScheme::Ecdsa {
            #[cfg(feature = "log")]
            error!("Cannot convert Signature to EcdsaSignature due to scheme mismatch. SigningScheme: {:?}", value.scheme);
            return Err(SecurityError::InvalidScheme);
        }

        Ok(EcdsaSignature::from_slice(value.as_bytes()).map_err(|_e| {
            #[cfg(feature = "log")]
            error!("Failed to construct a EcdsaSignature due to: {:?}", _e);
            SecurityError::SignatureMismatch
        })?)
    }
}
