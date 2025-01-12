use std::convert::Infallible;

use crate::{
    crypter::{Ivg, StatefulCrypter},
    io::{Io, Read, ReadAt, Seek, SeekFrom, Write, WriteAt},
    key::Key,
};

use super::Error;

/// You should probably only use this writing or reading
/// the entirety of IO (or with a BufReader),
/// as it uses one IV and thus usually requires a full read/write anyways
pub struct OneshotCryptIo<'a, IO, G, C, const KEY_SZ: usize> {
    pub io: IO,
    key: Key<KEY_SZ>,
    ivg: &'a mut G,
    crypter: &'a C,
}

impl<'a, IO, G, C, const KEY_SZ: usize> OneshotCryptIo<'a, IO, G, C, KEY_SZ> {
    pub fn new(io: IO, key: Key<KEY_SZ>, ivg: &'a mut G, crypter: &'a C) -> Self {
        Self {
            io,
            key,
            ivg,
            crypter,
        }
    }
}

impl<'a, IO, G, C, const KEY_SZ: usize> Io for OneshotCryptIo<'a, IO, G, C, KEY_SZ>
where
    IO: Io,
    G: Ivg,
    C: StatefulCrypter,
{
    type Error = Error<IO::Error, C::Error, G::Error, Infallible>;
}

impl<'a, IO, G, C, const KEY_SZ: usize> Read for OneshotCryptIo<'a, IO, G, C, KEY_SZ>
where
    IO: Read + Seek,
    G: Ivg,
    C: StatefulCrypter,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let origin = self.io.stream_position().map_err(Error::IO)?;

        // Read the current iv
        let mut iv = vec![0; C::iv_length()];
        self.io.seek(SeekFrom::Start(0)).map_err(Error::IO)?;
        match self.io.read_exact(&mut iv) {
            Ok(()) => {}
            Err(_) => {
                return Ok(0);
            }
        };

        // Read the rest of the file.
        let mut all = vec![];
        let n = self.io.read_to_end(&mut all).map_err(Error::IO)?;

        // Decrypt the file.
        self.crypter
            .decrypt(&self.key, &iv, &mut all)
            .map_err(Error::Crypter)?;

        let start = (origin as usize).min(n);
        let end = (start + buf.len()).min(n);
        let len = end - start;

        buf[0..len].copy_from_slice(&all[start..end]);

        self.io
            .seek(SeekFrom::Start(origin + len as u64))
            .map_err(Error::IO)?;

        Ok(len)
    }

    // fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize, Self::Error> {
    //     // Read the complete iv + ciphertext
    //     let cn = self.io.read_to_end(buf)?;
    //     if cn < FAKE_IV_LENGTH {
    //         return Ok(0);
    //     }

    //     // Decrypt it
    //     // let iv = buf[..FAKE_IV_LENGTH].to_vec();
    //     // let plaintext = C::decrypt(&self.key, &iv, &mut buf[FAKE_IV_LENGTH..])
    //     //     .map_err(|_| ())
    //     //     .unwrap();
    //     let (iv, pt) = buf.split_at_mut(FAKE_IV_LENGTH);
    //     self.crypter
    //         .decrypt(&self.key, iv, pt)
    //         .map_err(|_| ())
    //         .unwrap();

    //     // Copy the plaintext back into buf
    //     *buf = pt.to_vec();

    //     Ok(buf.len())
    // }
}

impl<'a, IO, G, C, const KEY_SZ: usize> ReadAt for OneshotCryptIo<'a, IO, G, C, KEY_SZ>
where
    IO: ReadAt,
    G: Ivg,
    C: StatefulCrypter,
{
    fn read_at(&mut self, buf: &mut [u8], offset: u64) -> Result<usize, Self::Error> {
        // let start_pos = offset;

        // Read the current iv
        let mut iv = vec![0; C::iv_length()];
        match self.io.read_exact_at(&mut iv, 0) {
            Ok(()) => {}
            Err(_) => {
                return Ok(0);
            }
        };

        // Read the rest of the file.
        let mut all = vec![];
        let n = self
            .io
            .read_to_end_at(&mut all, iv.len() as u64)
            .map_err(Error::IO)?;

        // Decrypt the file.
        self.crypter
            .decrypt(&self.key, &iv, &mut all)
            .map_err(Error::Crypter)?;

        let start = (offset as usize).min(n);
        let end = (start + buf.len()).min(n);
        let len = end - start;

        buf[0..len].copy_from_slice(&all[start..end]);

        Ok(len)
    }

    // fn read_to_end_at(&mut self, buf: &mut Vec<u8>, offset: u64) -> Result<usize, Self::Error> {
    //     // Read the complete iv + ciphertext
    //     let cn = self.io.read_to_end_at(buf, offset)?;
    //     if cn < FAKE_IV_LENGTH {
    //         return Ok(0);
    //     }

    //     // Decrypt it
    //     let (iv, pt) = buf.split_at_mut(FAKE_IV_LENGTH);
    //     self.crypter
    //         .decrypt(&self.key, iv, pt)
    //         .map_err(|_| ())
    //         .unwrap();

    //     // Copy the plaintext back into buf
    //     *buf = pt.to_vec();

    //     Ok(buf.len())
    // }
}

impl<'a, IO, G, C, const KEY_SZ: usize> Write for OneshotCryptIo<'a, IO, G, C, KEY_SZ>
where
    IO: Read + Write + Seek,
    G: Ivg,
    C: StatefulCrypter,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // TODO: this implementation is lazzzzy

        let start_pos = self.io.stream_position().map_err(Error::IO)?;

        // Read the currently existent iv + ciphertext
        let mut data = vec![0; C::iv_length()];
        // get length of file
        // let len = self.io.seek(SeekFrom::End(0)).map_err(Error::IO)?;
        // self.io.seek(SeekFrom::Start(0)).map_err(Error::IO)?;
        let cn = self.io.read_to_end(&mut data).map_err(Error::IO)?;

        let mut plaintext = if cn <= C::iv_length() {
            // If the initial file was too empty, then the plaintext is just buf.
            buf.to_vec()
        } else {
            // Otherwise, decrypt the ciphertext.
            let (iv, ct) = data.split_at_mut(C::iv_length());

            self.crypter
                .decrypt(&self.key, iv, ct)
                .map_err(Error::Crypter)?;

            let mut pt = ct.to_vec();

            // And substitute in the to-be-written data
            let sub_bytes = buf.len().min(pt.len() - start_pos as usize);
            pt[start_pos as usize..start_pos as usize + sub_bytes]
                .copy_from_slice(&buf[..sub_bytes]);
            pt.extend(&buf[sub_bytes..]);

            pt
        };

        // Generate the new IV.
        let mut new_iv = vec![0; C::iv_length()];
        self.ivg.gen(&mut new_iv).map_err(Error::IV)?;

        self.io.seek(SeekFrom::Start(0)).map_err(Error::IO)?;
        self.io.write_all(&new_iv).map_err(Error::IO)?;

        // Encrypt the plaintext and write it.
        self.crypter
            .encrypt(&self.key, &new_iv, &mut plaintext)
            .map_err(Error::Crypter)?;

        self.io.write_all(&plaintext).map_err(Error::IO)?;

        // Restore cursor position.
        self.io
            .seek(SeekFrom::Start(start_pos + buf.len() as u64))
            .map_err(Error::IO)?;

        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.io.flush().map_err(Error::IO)
    }
}

impl<'a, IO, G, C, const KEY_SZ: usize> WriteAt for OneshotCryptIo<'a, IO, G, C, KEY_SZ>
where
    IO: ReadAt + WriteAt,
    G: Ivg,
    C: StatefulCrypter,
{
    fn write_at(&mut self, buf: &[u8], offset: u64) -> Result<usize, Self::Error> {
        // TODO: this implementation is lazzzzy

        let start_pos = offset;

        // Read the currently existent iv + ciphertext
        let mut data = Vec::new();
        let cn = self.io.read_to_end_at(&mut data, 0).map_err(Error::IO)?;

        let mut plaintext = if cn <= C::iv_length() {
            // If the initial file was too empty, then the plaintext is just buf.
            buf.to_vec()
        } else {
            // Otherwise, decrypt the ciphertext.
            let (iv, ct) = data.split_at_mut(C::iv_length());

            // plaintext_b =
            //     C::decrypt(&self.key, &data[..FAKE_IV_LENGTH], &data[FAKE_IV_LENGTH..])
            //         .map_err(|_| ())
            //         .unwrap();
            self.crypter
                .decrypt(&self.key, &iv, ct)
                .map_err(Error::Crypter)?;

            let mut pt = ct.to_vec();

            // And substitute in the to-be-written data
            let sub_bytes = buf.len().min(pt.len() - start_pos as usize);
            pt[start_pos as usize..start_pos as usize + sub_bytes]
                .copy_from_slice(&buf[..sub_bytes]);
            pt.extend(&buf[sub_bytes..]);

            pt
        };

        // Generate the new IV.
        let mut new_iv = vec![0; C::iv_length()];
        self.ivg.gen(&mut new_iv).map_err(Error::IV)?;
        self.io.write_all_at(&new_iv, 0).map_err(Error::IO)?;

        // Encrypt the plaintext and write it.
        self.crypter
            .encrypt(&self.key, &new_iv, &mut plaintext)
            .map_err(Error::Crypter)?;

        self.io
            .write_all_at(&plaintext, new_iv.len() as u64)
            .map_err(Error::IO)?;

        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.io.flush().map_err(Error::IO)
    }
}

impl<'a, IO, G, C, const KEY_SZ: usize> Seek for OneshotCryptIo<'a, IO, G, C, KEY_SZ>
where
    IO: Seek,
    C: StatefulCrypter,
    G: Ivg,
{
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        self.io.seek(pos).map_err(Error::IO)
    }
}

// #[cfg(test)]
// mod tests {
//     use anyhow::Result;
//     use rand::rngs::ThreadRng;

//     use crate::{
//         consts::KEY_SIZE,
//         crypter::{aes::Aes256Ctr, ivs::SequentialIvg},
//         io::stdio::StdIo,
//         key::KeyGenerator,
//     };

//     use super::*;

//     #[test]
//     fn oneshot() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let mut ivg = SequentialIvg::default();
//         let crypter = Aes256Ctr::new();

//         let key = rng.gen_key();

//         let mut io =
//             OneshotCryptIo::<StdIo<NamedTempFile>, SequentialIvg, Aes256Ctr, KEY_SIZE>::new(
//                 StdIo::new(NamedTempFile::new()?),
//                 key,
//                 &mut ivg,
//                 &crypter,
//             );

//         let data1 = vec!['a' as u8; 8192];
//         io.seek(SeekFrom::Start(0))?;
//         io.write_all(&data1)?;

//         let mut data2 = vec![];
//         io.seek(SeekFrom::Start(0))?;
//         io.read_to_end(&mut data2)?;

//         assert_eq!(data1, data2);

//         Ok(())
//     }

//     #[test]
//     fn oneshot_at() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let mut ivg = SequentialIvg::default();
//         let crypter = Aes256Ctr::new();

//         let key = rng.gen_key();

//         let mut io =
//             OneshotCryptIo::<StdIo<NamedTempFile>, SequentialIvg, Aes256Ctr, KEY_SIZE>::new(
//                 StdIo::new(NamedTempFile::new()?),
//                 key,
//                 &mut ivg,
//                 &crypter,
//             );

//         let data1 = vec!['a' as u8; 8192];
//         io.write_all_at(&data1, 0)?;

//         let mut data2 = vec![];
//         io.read_to_end_at(&mut data2, 0)?;

//         assert_eq!(data1, data2);

//         Ok(())
//     }

//     #[test]
//     fn overwrite() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let mut ivg = SequentialIvg::default();
//         let crypter = Aes256Ctr::new();

//         let key = rng.gen_key();

//         let mut io =
//             OneshotCryptIo::<StdIo<NamedTempFile>, SequentialIvg, Aes256Ctr, KEY_SIZE>::new(
//                 StdIo::new(NamedTempFile::new()?),
//                 key,
//                 &mut ivg,
//                 &crypter,
//             );

//         let xs = vec!['a' as u8; 8192];
//         let ys = vec!['b' as u8; 8192];

//         io.seek(SeekFrom::Start(0))?;
//         io.write_all(&xs)?;

//         io.seek(SeekFrom::Start(3))?;
//         io.write_all(&ys)?;

//         let mut data = vec![];
//         io.seek(SeekFrom::Start(0))?;
//         io.read_to_end(&mut data)?;

//         assert_eq!(&data[0..3], &xs[0..3]);
//         assert_eq!(&data[3..], &ys);
//         assert_eq!(data.len(), ys.len() + 3);

//         Ok(())
//     }

//     #[test]
//     fn overwrite_at() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let mut ivg = SequentialIvg::default();
//         let crypter = Aes256Ctr::new();

//         let key = rng.gen_key();

//         let mut io =
//             OneshotCryptIo::<StdIo<NamedTempFile>, SequentialIvg, Aes256Ctr, KEY_SIZE>::new(
//                 StdIo::new(NamedTempFile::new()?),
//                 key,
//                 &mut ivg,
//                 &crypter,
//             );

//         let xs = vec!['a' as u8; 8192];
//         let ys = vec!['b' as u8; 8192];

//         io.write_all_at(&xs, 0)?;
//         io.write_all_at(&ys, 3)?;

//         let mut data = vec![];
//         io.read_to_end_at(&mut data, 0)?;

//         assert_eq!(&data[0..3], &xs[0..3]);
//         assert_eq!(&data[3..], &ys);
//         assert_eq!(data.len(), ys.len() + 3);

//         Ok(())
//     }

//     #[test]
//     fn append() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let mut ivg = SequentialIvg::default();
//         let crypter = Aes256Ctr::new();

//         let key = rng.gen_key();

//         let mut io =
//             OneshotCryptIo::<StdIo<NamedTempFile>, SequentialIvg, Aes256Ctr, KEY_SIZE>::new(
//                 StdIo::new(NamedTempFile::new()?),
//                 key,
//                 &mut ivg,
//                 &crypter,
//             );

//         let xs = vec!['a' as u8; 8192];
//         let ys = vec!['b' as u8; 8192];

//         io.seek(SeekFrom::Start(0))?;
//         io.write_all(&xs)?;
//         io.write_all(&ys)?;

//         let mut data = vec![];
//         io.seek(SeekFrom::Start(0))?;
//         io.read_to_end(&mut data)?;

//         assert_eq!(&data[..xs.len()], &xs);
//         assert_eq!(&data[xs.len()..], &ys);
//         assert_eq!(data.len(), xs.len() + ys.len());

//         Ok(())
//     }

//     #[test]
//     fn append_at() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let mut ivg = SequentialIvg::default();
//         let crypter = Aes256Ctr::new();

//         let key = rng.gen_key();

//         let mut io =
//             OneshotCryptIo::<StdIo<NamedTempFile>, SequentialIvg, Aes256Ctr, KEY_SIZE>::new(
//                 StdIo::new(NamedTempFile::new()?),
//                 key,
//                 &mut ivg,
//                 &crypter,
//             );

//         let xs = vec!['a' as u8; 8192];
//         let ys = vec!['b' as u8; 8192];

//         io.write_all_at(&xs, 0)?;
//         io.write_all_at(&ys, xs.len() as u64)?;

//         let mut data = vec![];
//         io.read_to_end_at(&mut data, 0)?;

//         assert_eq!(&data[..xs.len()], &xs);
//         assert_eq!(&data[xs.len()..], &ys);
//         assert_eq!(data.len(), xs.len() + ys.len());

//         Ok(())
//     }
// }
