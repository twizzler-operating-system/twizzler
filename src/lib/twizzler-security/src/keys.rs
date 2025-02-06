use p256::ecdsa::SigningKey;

use crate::{CapError, SigningScheme};

#[derive(Clone, Copy)]
// making our own struct for verifying key since we need to be able to support keys with different
// schemes, (meaning they could also be different lengths)
pub struct VerifyingKey {
    key: [u8; 1024],
    len: u16,
    pub scheme: SigningScheme,
}
impl VerifyingKey {
    pub fn new(scheme: SigningScheme, target_priv_key: &[u8]) -> Result<Self, CapError> {
        match scheme {
            SigningScheme::Ecdsa => {
                let signing_key = SigningKey::from_slice(target_priv_key)
                    .map_err(|_| CapError::InvalidPrivateKey)?;
                let vkey = p256::ecdsa::VerifyingKey::from(signing_key);
                let mut buff = [0; 1024];
                let len = 33;
                buff[0..len].copy_from_slice(vkey.to_encoded_point(true).as_bytes());
                Ok(VerifyingKey {
                    key: buff,
                    len: len as u16,
                    scheme,
                })
            }
        }
    }
    // so we can easily extract out the key without worrying about len and the buffer
    pub fn as_bytes(&self) -> &[u8] {
        &self.key[0..self.len as usize]
    }
}
