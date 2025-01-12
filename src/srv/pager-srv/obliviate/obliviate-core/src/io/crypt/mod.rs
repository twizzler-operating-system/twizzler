mod error;
// mod journaled;
// mod keyed;
mod oneshot;
// mod scrape;
// mod speculative;
// mod versioned;

pub use {
    error::Error,
    // journaled::JournaledPreCryptAt,
    oneshot::OneshotCryptIo,
    // scrape::ModifiedBlockScraper,
    // speculative::SpeculativePreCryptAt,
    // versioned::VersionedPreCryptAt,
};

// #[cfg(test)]
// mod testing {
//     use crate::{consts::KEY_SIZE, key::Key};

//     pub const ROOT_KEY: Key<KEY_SIZE> = [0; KEY_SIZE];

//     macro_rules! cryptio_padded_test_impl {
//         ($name:expr, $confgen:tt, $iogen:tt, $padlen:expr) => {
//             // Writes 1 block of 'a's.
//             #[test]
//             fn simple() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_simple", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; BLOCK_SIZE])?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..], &['a' as u8; BLOCK_SIZE]);

//                 Ok(())
//             }

//             // Writes 1 block of 'a's.
//             #[test]
//             fn simple_at() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_simple_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; BLOCK_SIZE], 0)?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..], &['a' as u8; BLOCK_SIZE]);

//                 Ok(())
//             }

//             // Writes 4 blocks of 'a's, then 4 'b's at offset 3.
//             #[test]
//             fn offset_write() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_offset_write", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; 4 * BLOCK_SIZE])?;
//                 blockio.seek(SeekFrom::Start(3))?;
//                 blockio.write_all(&['b' as u8; 4])?;

//                 let mut buf = vec![0; 4 * BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..3], &['a' as u8; 3]);
//                 assert_eq!(&buf[3..7], &['b' as u8; 4]);
//                 assert_eq!(&buf[7..], &['a' as u8; 4 * BLOCK_SIZE - 7]);

//                 Ok(())
//             }

//             // Writes 4 blocks of 'a's, then 4 'b's at offset 3.
//             #[test]
//             fn offset_write_at() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_offset_write_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; 4 * BLOCK_SIZE], 0)?;
//                 blockio.write_all_at(&['b' as u8; 4], 3)?;

//                 let mut buf = vec![0; 4 * BLOCK_SIZE];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..3], &['a' as u8; 3]);
//                 assert_eq!(&buf[3..7], &['b' as u8; 4]);
//                 assert_eq!(&buf[7..], &['a' as u8; 4 * BLOCK_SIZE - 7]);

//                 Ok(())
//             }

//             // Writes 2 blocks of 'a's and a block of 'b' right in the middle.
//             #[test]
//             fn misaligned_write() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_misaligned_write", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; 2 * BLOCK_SIZE])?;
//                 blockio.seek(SeekFrom::Start((BLOCK_SIZE / 2) as u64))?;
//                 blockio.write_all(&['b' as u8; BLOCK_SIZE])?;

//                 let mut buf = vec![0; 2 * BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..BLOCK_SIZE / 2], &['a' as u8; BLOCK_SIZE / 2]);
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2..BLOCK_SIZE / 2 + BLOCK_SIZE],
//                     &['b' as u8; BLOCK_SIZE]
//                 );
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2 + BLOCK_SIZE..],
//                     &['a' as u8; BLOCK_SIZE / 2]
//                 );

//                 Ok(())
//             }

//             // Writes 2 blocks of 'a's and a block of 'b' right in the middle.
//             #[test]
//             fn misaligned_write_at() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_{}_test_misaligned_write_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; 2 * BLOCK_SIZE], 0)?;
//                 blockio.write_all_at(&['b' as u8; BLOCK_SIZE], (BLOCK_SIZE / 2) as u64)?;

//                 let mut buf = vec![0; 2 * BLOCK_SIZE];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..BLOCK_SIZE / 2], &['a' as u8; BLOCK_SIZE / 2]);
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2..BLOCK_SIZE / 2 + BLOCK_SIZE],
//                     &['b' as u8; BLOCK_SIZE]
//                 );
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2 + BLOCK_SIZE..],
//                     &['a' as u8; BLOCK_SIZE / 2]
//                 );

//                 Ok(())
//             }

//             #[test]
//             fn short_write() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_short_write", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8])?;
//                 blockio.write_all(&['b' as u8])?;

//                 let mut buf = vec![0; 2];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..], &['a' as u8, 'b' as u8]);

//                 Ok(())
//             }

//             #[test]
//             fn short_write_at() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_short_write_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8], 0)?;
//                 blockio.write_all_at(&['b' as u8], 1)?;

//                 let mut buf = vec![0; 2];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..], &['a' as u8, 'b' as u8]);

//                 Ok(())
//             }

//             #[test]
//             fn read_too_much() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_read_too_much", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; 16])?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 let n = blockio.read(&mut buf)?;

//                 assert_eq!(n, 16);
//                 assert_eq!(&buf[..n], &['a' as u8; 16]);

//                 Ok(())
//             }

//             #[test]
//             fn read_too_much_at() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_{}_test_read_too_much_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; 16], 0)?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 let n = blockio.read_at(&mut buf, 0)?;

//                 assert_eq!(n, 16);
//                 assert_eq!(&buf[..n], &['a' as u8; 16]);

//                 Ok(())
//             }

//             #[test]
//             fn random() -> anyhow::Result<()> {
//                 for _ in 0..20 {
//                     let (mut config, _) = $confgen(&format!("cryptio_{}_test_random", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let nbytes = rng.gen::<usize>() % (1 << 16);
//                     let mut pt = vec![0; nbytes];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all(&pt)?;

//                     let mut xt = vec![0; pt.len()];
//                     blockio.seek(SeekFrom::Start(0).into())?;
//                     let n = blockio.read(&mut xt)?;

//                     assert_eq!(n, pt.len());
//                     assert_eq!(pt, xt);
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn random_at() -> anyhow::Result<()> {
//                 for _ in 0..20 {
//                     let (mut config, _) = $confgen(&format!("cryptio_{}_test_random_at", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let nbytes = rng.gen::<usize>() % (1 << 16);
//                     let mut pt = vec![0; nbytes];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all_at(&pt, 0)?;

//                     let mut xt = vec![0; pt.len()];
//                     let n = blockio.read_at(&mut xt, 0)?;

//                     assert_eq!(n, pt.len());
//                     assert_eq!(pt, xt);
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn sequential() -> anyhow::Result<()> {
//                 for _ in 0..10 {
//                     let (mut config, _) = $confgen(&format!("cryptio_{}_test_sequential", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let mut pt = vec![0; BLOCK_SIZE];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all(&pt)?;

//                     blockio.seek(SeekFrom::Start(0).into())?;
//                     let mut xt = [0];

//                     for c in &pt {
//                         let n = blockio.read(&mut xt)?;
//                         assert_eq!(n, 1);
//                         assert_eq!(*c, xt[0]);
//                     }
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn sequential_at() -> anyhow::Result<()> {
//                 for _ in 0..10 {
//                     let (mut config, _) =
//                         $confgen(&format!("cryptio_{}_test_sequential_at", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let mut pt = vec![0; BLOCK_SIZE];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all_at(&pt, 0)?;

//                     let mut xt = [0];

//                     for (i, c) in pt.iter().enumerate() {
//                         let n = blockio.read_at(&mut xt, i as u64)?;
//                         assert_eq!(n, 1);
//                         assert_eq!(*c, xt[0]);
//                     }
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn correctness() -> anyhow::Result<()> {
//                 let (mut config, file_path) =
//                     $confgen(&format!("cryptio_{}_test_correctness", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let mut n = 0;
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 n += blockio.write(&['a' as u8; 7])?;
//                 blockio.seek(SeekFrom::Start(7).into())?;
//                 n += blockio.write(&['b' as u8; 29])?;

//                 let mut buf = vec![0; 36];
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 blockio.read(&mut buf[0..7])?;
//                 blockio.read(&mut buf[7..36])?;

//                 assert_eq!(n, 36);
//                 assert_eq!(fs::metadata(&file_path)?.len(), 36 + $padlen as u64);
//                 assert_eq!(&buf[0..7], &['a' as u8; 7]);
//                 assert_eq!(&buf[7..36], &['b' as u8; 29]);

//                 Ok(())
//             }

//             #[test]
//             fn correctness_at() -> anyhow::Result<()> {
//                 let (mut config, file_path) =
//                     $confgen(&format!("cryptio_{}_test_correctness_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let mut n = 0;
//                 n += blockio.write_at(&['a' as u8; 7], 0)?;
//                 n += blockio.write_at(&['b' as u8; 29], 7)?;

//                 let mut buf = vec![0; 36];
//                 blockio.read_at(&mut buf[0..7], 0)?;
//                 blockio.read_at(&mut buf[7..36], 7)?;

//                 assert_eq!(n, 36);
//                 assert_eq!(fs::metadata(&file_path)?.len(), 36 + $padlen as u64);
//                 assert_eq!(&buf[0..7], &['a' as u8; 7]);
//                 assert_eq!(&buf[7..36], &['b' as u8; 29]);

//                 Ok(())
//             }

//             #[test]
//             fn short() -> anyhow::Result<()> {
//                 let (mut config, file_path) = $confgen(&format!("cryptio_{}_test_short", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let stuff = ['a' as u8; 24];
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 let n = blockio.write(&stuff)?;
//                 blockio.seek(SeekFrom::Start(0).into())?;

//                 eprintln!("using 400B buffer");
//                 let mut data = vec![0; 400];
//                 let m = blockio.read(&mut data)?;

//                 assert_eq!(n, 24);
//                 assert_eq!(m, 24);
//                 assert_eq!(&data[..n], &stuff);
//                 assert_eq!(fs::metadata(&file_path)?.len(), m as u64 + $padlen as u64);

//                 Ok(())
//             }

//             #[test]
//             fn short_at() -> anyhow::Result<()> {
//                 let (mut config, file_path) = $confgen(&format!("cryptio_{}_test_short_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let n = blockio.write_at(&['a' as u8; 24], 0)?;

//                 let mut data = vec![0; 400];
//                 let m = blockio.read_at(&mut data, 0)?;

//                 assert_eq!(n, 24);
//                 assert_eq!(m, 24);
//                 assert_eq!(&data[..n], &['a' as u8; 24]);
//                 assert_eq!(fs::metadata(&file_path)?.len(), m as u64 + $padlen as u64);

//                 Ok(())
//             }
//         };
//     }

//     macro_rules! cryptio_unpadded_test_impl {
//         ($name:expr, $confgen:tt, $iogen:tt) => {
//             // Writes 1 block of 'a's.
//             #[test]
//             fn simple() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!("cryptio_unpadded_{}_test_simple", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; BLOCK_SIZE])?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..], &['a' as u8; BLOCK_SIZE]);

//                 Ok(())
//             }

//             // Writes 1 block of 'a's.
//             #[test]
//             fn simple_at() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_simple_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; BLOCK_SIZE], 0)?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..], &['a' as u8; BLOCK_SIZE]);

//                 Ok(())
//             }

//             // Writes 4 blocks of 'a's, then 4 'b's at offset 3.
//             #[test]
//             fn offset_write() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_offset_write", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; 4 * BLOCK_SIZE])?;
//                 blockio.seek(SeekFrom::Start(3))?;
//                 blockio.write_all(&['b' as u8; 4])?;

//                 let mut buf = vec![0; 4 * BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..3], &['a' as u8; 3]);
//                 assert_eq!(&buf[3..7], &['b' as u8; 4]);
//                 assert_eq!(&buf[7..], &['a' as u8; 4 * BLOCK_SIZE - 7]);

//                 Ok(())
//             }

//             // Writes 4 blocks of 'a's, then 4 'b's at offset 3.
//             #[test]
//             fn offset_write_at() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_offset_write_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; 4 * BLOCK_SIZE], 0)?;
//                 blockio.write_all_at(&['b' as u8; 4], 3)?;

//                 let mut buf = vec![0; 4 * BLOCK_SIZE];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..3], &['a' as u8; 3]);
//                 assert_eq!(&buf[3..7], &['b' as u8; 4]);
//                 assert_eq!(&buf[7..], &['a' as u8; 4 * BLOCK_SIZE - 7]);

//                 Ok(())
//             }

//             // Writes 2 blocks of 'a's and a block of 'b' right in the middle.
//             #[test]
//             fn misaligned_write() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_misaligned_write", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; 2 * BLOCK_SIZE])?;
//                 blockio.seek(SeekFrom::Start((BLOCK_SIZE / 2) as u64))?;
//                 blockio.write_all(&['b' as u8; BLOCK_SIZE])?;

//                 let mut buf = vec![0; 2 * BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..BLOCK_SIZE / 2], &['a' as u8; BLOCK_SIZE / 2]);
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2..BLOCK_SIZE / 2 + BLOCK_SIZE],
//                     &['b' as u8; BLOCK_SIZE]
//                 );
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2 + BLOCK_SIZE..],
//                     &['a' as u8; BLOCK_SIZE / 2]
//                 );

//                 Ok(())
//             }

//             // Writes 2 blocks of 'a's and a block of 'b' right in the middle.
//             #[test]
//             fn misaligned_write_at() -> anyhow::Result<()> {
//                 let (mut config, _) = $confgen(&format!(
//                     "cryptio_unpadded_{}_test_misaligned_write_at",
//                     $name
//                 ));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; 2 * BLOCK_SIZE], 0)?;
//                 blockio.write_all_at(&['b' as u8; BLOCK_SIZE], (BLOCK_SIZE / 2) as u64)?;

//                 let mut buf = vec![0; 2 * BLOCK_SIZE];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..BLOCK_SIZE / 2], &['a' as u8; BLOCK_SIZE / 2]);
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2..BLOCK_SIZE / 2 + BLOCK_SIZE],
//                     &['b' as u8; BLOCK_SIZE]
//                 );
//                 assert_eq!(
//                     &buf[BLOCK_SIZE / 2 + BLOCK_SIZE..],
//                     &['a' as u8; BLOCK_SIZE / 2]
//                 );

//                 Ok(())
//             }

//             #[test]
//             fn short_write() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_short_write", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8])?;
//                 blockio.write_all(&['b' as u8])?;

//                 let mut buf = vec![0; 2];
//                 blockio.seek(SeekFrom::Start(0))?;
//                 blockio.read_exact(&mut buf)?;

//                 assert_eq!(&buf[..], &['a' as u8, 'b' as u8]);

//                 Ok(())
//             }

//             #[test]
//             fn short_write_at() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_short_write_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8], 0)?;
//                 blockio.write_all_at(&['b' as u8], 1)?;

//                 let mut buf = vec![0; 2];
//                 blockio.read_exact_at(&mut buf, 0)?;

//                 assert_eq!(&buf[..], &['a' as u8, 'b' as u8]);

//                 Ok(())
//             }

//             #[test]
//             fn read_too_much() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_read_too_much", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all(&['a' as u8; 16])?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 let n = blockio.read(&mut buf)?;

//                 assert_eq!(n, 16);
//                 assert_eq!(&buf[..n], &['a' as u8; 16]);

//                 Ok(())
//             }

//             #[test]
//             fn read_too_much_at() -> anyhow::Result<()> {
//                 let (mut config, _) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_read_too_much_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 blockio.write_all_at(&['a' as u8; 16], 0)?;

//                 let mut buf = vec![0; BLOCK_SIZE];
//                 let n = blockio.read_at(&mut buf, 0)?;

//                 assert_eq!(n, 16);
//                 assert_eq!(&buf[..n], &['a' as u8; 16]);

//                 Ok(())
//             }

//             #[test]
//             fn random() -> anyhow::Result<()> {
//                 for _ in 0..20 {
//                     let (mut config, _) =
//                         $confgen(&format!("cryptio_unpadded_{}_test_random", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let nbytes = rng.gen::<usize>() % (1 << 16);
//                     let mut pt = vec![0; nbytes];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all(&pt)?;

//                     let mut xt = vec![0; pt.len()];
//                     blockio.seek(SeekFrom::Start(0).into())?;
//                     let n = blockio.read(&mut xt)?;

//                     assert_eq!(n, pt.len());
//                     assert_eq!(pt, xt);
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn random_at() -> anyhow::Result<()> {
//                 for _ in 0..20 {
//                     let (mut config, _) =
//                         $confgen(&format!("cryptio_unpadded_{}_test_random_at", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let nbytes = rng.gen::<usize>() % (1 << 16);
//                     let mut pt = vec![0; nbytes];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all_at(&pt, 0)?;

//                     let mut xt = vec![0; pt.len()];
//                     let n = blockio.read_at(&mut xt, 0)?;

//                     assert_eq!(n, pt.len());
//                     assert_eq!(pt, xt);
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn sequential() -> anyhow::Result<()> {
//                 for _ in 0..10 {
//                     let (mut config, _) =
//                         $confgen(&format!("cryptio_unpadded_{}_test_sequential", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let mut pt = vec![0; BLOCK_SIZE];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all(&pt)?;

//                     blockio.seek(SeekFrom::Start(0).into())?;
//                     let mut xt = [0];

//                     for c in &pt {
//                         let n = blockio.read(&mut xt)?;
//                         assert_eq!(n, 1);
//                         assert_eq!(*c, xt[0]);
//                     }
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn sequential_at() -> anyhow::Result<()> {
//                 for _ in 0..10 {
//                     let (mut config, _) =
//                         $confgen(&format!("cryptio_unpadded_{}_test_sequential_at", $name));
//                     let mut blockio = $iogen(&mut config);

//                     let mut rng = ThreadRng::default();
//                     let mut pt = vec![0; BLOCK_SIZE];
//                     rng.fill_bytes(&mut pt);

//                     blockio.write_all_at(&pt, 0)?;

//                     let mut xt = [0];

//                     for (i, c) in pt.iter().enumerate() {
//                         let n = blockio.read_at(&mut xt, i as u64)?;
//                         assert_eq!(n, 1);
//                         assert_eq!(*c, xt[0]);
//                     }
//                 }

//                 Ok(())
//             }

//             #[test]
//             fn correctness() -> anyhow::Result<()> {
//                 let (mut config, file_path) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_correctness", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let mut n = 0;
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 n += blockio.write(&['a' as u8; 7])?;
//                 blockio.seek(SeekFrom::Start(7).into())?;
//                 n += blockio.write(&['b' as u8; 29])?;

//                 let mut buf = vec![0; 36];
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 blockio.read(&mut buf[0..7])?;
//                 blockio.read(&mut buf[7..36])?;

//                 assert_eq!(n, 36);
//                 assert_eq!(fs::metadata(&file_path)?.len(), 36);
//                 assert_eq!(&buf[0..7], &['a' as u8; 7]);
//                 assert_eq!(&buf[7..36], &['b' as u8; 29]);

//                 Ok(())
//             }

//             #[test]
//             fn correctness_at() -> anyhow::Result<()> {
//                 let (mut config, file_path) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_correctness_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let mut n = 0;
//                 n += blockio.write_at(&['a' as u8; 7], 0)?;
//                 n += blockio.write_at(&['b' as u8; 29], 7)?;

//                 let mut buf = vec![0; 36];
//                 blockio.read_at(&mut buf[0..7], 0)?;
//                 blockio.read_at(&mut buf[7..36], 7)?;

//                 assert_eq!(n, 36);
//                 assert_eq!(fs::metadata(&file_path)?.len(), 36);
//                 assert_eq!(&buf[0..7], &['a' as u8; 7]);
//                 assert_eq!(&buf[7..36], &['b' as u8; 29]);

//                 Ok(())
//             }

//             #[test]
//             fn short() -> anyhow::Result<()> {
//                 let (mut config, file_path) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_short", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let stuff = ['a' as u8; 24];
//                 blockio.seek(SeekFrom::Start(0).into())?;
//                 let n = blockio.write(&stuff)?;
//                 blockio.seek(SeekFrom::Start(0).into())?;

//                 let mut data = vec![0; 400];
//                 let m = blockio.read(&mut data)?;

//                 assert_eq!(n, 24);
//                 assert_eq!(m, 24);
//                 assert_eq!(&data[..n], &stuff);
//                 assert_eq!(fs::metadata(&file_path)?.len(), m as u64);

//                 Ok(())
//             }

//             #[test]
//             fn short_at() -> anyhow::Result<()> {
//                 let (mut config, file_path) =
//                     $confgen(&format!("cryptio_unpadded_{}_test_short_at", $name));
//                 let mut blockio = $iogen(&mut config);

//                 let n = blockio.write_at(&['a' as u8; 24], 0)?;

//                 let mut data = vec![0; 400];
//                 let m = blockio.read_at(&mut data, 0)?;

//                 assert_eq!(n, 24);
//                 assert_eq!(m, 24);
//                 assert_eq!(&data[..n], &['a' as u8; 24]);
//                 assert_eq!(fs::metadata(&file_path)?.len(), m as u64);

//                 Ok(())
//             }
//         };
//     }

//     pub(crate) use {cryptio_padded_test_impl, cryptio_unpadded_test_impl};
// }
