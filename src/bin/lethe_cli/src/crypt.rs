
use crypter::Crypter;

use orion::aead::{seal, open, SecretKey};

pub struct Oreo;

impl Crypter for Oreo {
    type Error = orion::errors::UnknownCryptoError;

    fn key_length() -> usize {
        32
    }


    fn iv_length() -> usize {
        16
    }

    fn encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let key = &SecretKey::from_slice(key)?;
        let ciphertext = seal(key, data).unwrap();

        Ok(ciphertext)
        //seal(&SecretKey::from_slice(key).unwrap(), data)
    }

    fn onetime_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Self::encrypt(key, &[0], data)
        //seal(&SecretKey::fro`m_slice(key).unwrap(), data)
    }

    fn decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let key = &SecretKey::from_slice(key)?;
        let ciphertext = open(key, data).unwrap();

        Ok(ciphertext)
    }

    fn onetime_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Self::decrypt(key, &[0], data)
    }
}

pub struct Water;

impl Crypter for Water {
    type Error = std::io::Error;

    fn key_length() -> usize {
        32
    }


    fn iv_length() -> usize {
        16
    }

    fn encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Ok(Vec::from(data))
    }

    fn onetime_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Ok(Vec::from(data))
    }

    fn decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Ok(Vec::from(data))
    }

    fn onetime_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Ok(Vec::from(data))
    }
}
