use std::convert::Infallible;

use aes::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr64LE;
use paste::paste;

use super::{Crypter, StatefulCrypter};

macro_rules! crypter_impl {
    ($crypter:ident,$keylen:expr,$blklen:expr,$ivlen:expr) => {
        paste! {
            type [<$crypter Ctr64LE>] = Ctr64LE<aes:: $crypter>;

            pub struct [<$crypter Ctr>];

            impl [<$crypter Ctr>] {
                pub fn new() -> Self {
                    Self {}
                }
            }

            impl Default for [<$crypter Ctr>] {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl Crypter for [<$crypter Ctr>] {
                type Error = Infallible;

                fn block_length() -> usize {
                    $blklen
                }

                fn iv_length() -> usize {
                    $ivlen
                }

                fn key_length() -> usize {
                    $keylen
                }

                fn encrypt(key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
                    let mut cipher = [<$crypter Ctr64LE>]::new(key.into(), iv.into());
                    cipher.apply_keystream(data);
                    Ok(())
                }

                fn decrypt(key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
                    let mut cipher = [<$crypter Ctr64LE>]::new(key.into(), iv.into());
                    cipher.apply_keystream(data);
                    Ok(())
                }
            }

            impl StatefulCrypter for [<$crypter Ctr>] {
                type Error = Infallible;

                fn block_length() -> usize {
                    $blklen
                }

                fn iv_length() -> usize {
                    $ivlen
                }

                fn key_length() -> usize {
                    $keylen
                }

                fn encrypt(&self, key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
                    let mut cipher = [<$crypter Ctr64LE>]::new(key.into(), iv.into());
                    cipher.apply_keystream(data);
                    Ok(())
                }

                fn decrypt(&self, key: &[u8], iv: &[u8], data: &mut [u8]) -> Result<(), Self::Error> {
                    let mut cipher = [<$crypter Ctr64LE>]::new(key.into(), iv.into());
                    cipher.apply_keystream(data);
                    Ok(())
                }
            }
        }
    };
}

crypter_impl!(Aes128, 16, 16, 16);
crypter_impl!(Aes192, 24, 16, 16);
crypter_impl!(Aes256, 32, 16, 16);

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use rand::{thread_rng, RngCore};

    macro_rules! crypter_test_impl {
        ($crypter:ident) => {
            paste! {
                #[test]
                fn [<$crypter:snake>]() -> Result<()> {
                    let mut key = vec![0; <[<$crypter Ctr>] as StatefulCrypter>::key_length()];
                    thread_rng().fill_bytes(&mut key);

                    let mut iv = vec![0; <[<$crypter Ctr>] as StatefulCrypter>::iv_length()];
                    thread_rng().fill_bytes(&mut iv);

                    let pt = b"this is a super secret message";
                    let mut ct = pt.to_vec();

                    let crypter = [<$crypter Ctr>]::new();

                    crypter.encrypt(&key, &iv, &mut ct)?;
                    assert_ne!(&pt[..], &ct[..]);

                    crypter.decrypt(&key, &iv, &mut ct)?;
                    assert_eq!(&pt[..], &ct[..]);

                    Ok(())
                }
            }
        };
    }

    crypter_test_impl!(Aes128);
    crypter_test_impl!(Aes192);
    crypter_test_impl!(Aes256);
}
