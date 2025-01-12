pub mod aes;
pub mod ivs;

pub trait Crypter {
    type Error: std::error::Error;

    fn key_length() -> usize;

    fn block_length() -> usize;

    fn iv_length() -> usize;

    fn encrypt(key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error>;

    fn onetime_encrypt(key: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
        Self::encrypt(key, &vec![0; Self::iv_length()], data)
    }

    fn decrypt(key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error>;

    fn onetime_decrypt(key: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
        Self::decrypt(key, &vec![0; Self::iv_length()], data)
    }
}

pub trait StatefulCrypter {
    type Error: std::error::Error;

    fn key_length() -> usize;

    fn block_length() -> usize;

    fn iv_length() -> usize;

    fn encrypt(&self, key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error>;

    fn onetime_encrypt(&self, key: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
        self.encrypt(key, &vec![0; Self::iv_length()], data)
    }

    fn decrypt(&self, key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error>;

    fn onetime_decrypt(&self, key: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
        self.decrypt(key, &vec![0; Self::iv_length()], data)
    }
}

pub trait Ivg {
    type Error: std::error::Error;

    fn gen(&mut self, iv: &mut [u8]) -> Result<(), Self::Error>;
}
