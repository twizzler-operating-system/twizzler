use std::{
    io::{self, Read, Write},
    marker::PhantomData,
    mem,
    ops::DerefMut,
    sync::{Mutex, RwLock, RwLockReadGuard},
};

use crc::{Crc, CRC_64_XZ};
use fatfs::{DefaultTimeProvider, FileSystem, IoBase, LossyOemCpConverter, ReadWriteSeek};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypter::{Ivg, StatefulCrypter},
    key::Key,
    syncfile::File,
};

#[derive(Error, Debug)]
pub enum SecureWALError<G, C, Io> {
    #[error(transparent)]
    StdIo(#[from] io::Error),

    #[error(transparent)]
    Serde(#[from] bincode::Error),

    #[error(transparent)]
    IV(G),

    #[error(transparent)]
    Crypt(C),

    #[error(transparent)]
    Fatfs(#[from] fatfs::Error<Io>),
}

pub struct SecureWAL<'a, IO: ReadWriteSeek, T, G, C, const N: usize> {
    /// The path to the WAL.
    path: String,
    /// The core state of the WAL.
    state: RwLock<State<T, G, N>>,
    /// Encrypts persisted WAL entries.
    crypter: C,
    /// Provides checksums for persisted WAL entries.
    crc: Crc<u64>,
    /// the filesystem to write into
    fs: &'a Mutex<FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>>,
}

struct State<T, G, const N: usize> {
    /// Contains the WAL's entries.
    entries: Vec<T>,
    /// Tracks the index of the first unpersisted WAL entry.
    cursor: usize,
    /// Provides IVs for persisted WAL entires.
    ivg: G,
}

unsafe impl<'a, IO, T, G, C, const N: usize> Sync for SecureWAL<'a, IO, T, G, C, N> where
    IO: ReadWriteSeek
{
}

impl<'a, IO, T, G, C, const N: usize> SecureWAL<'a, IO, T, G, C, N>
where
    IO: ReadWriteSeek + 'static + IoBase,
    std::io::Error: From<fatfs::Error<IO::Error>>,
{
    /// Opens a new WAL.
    pub fn open(
        path: String,
        key: Key<N>,
        fs: &'a Mutex<FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>>,
    ) -> Result<Self, SecureWALError<G::Error, C::Error, IO::Error>>
    where
        for<'de> T: Deserialize<'de>,
        G: Ivg + Default,
        C: StatefulCrypter + Default,
        IO: ReadWriteSeek + IoBase,
    {
        let state = State {
            entries: Vec::new(),
            cursor: 0,
            ivg: G::default(),
        };

        let wal = Self {
            path,
            state: RwLock::new(state),
            crypter: C::default(),
            crc: Crc::<u64>::new(&CRC_64_XZ),
            fs,
        };

        wal.load(key)?;

        Ok(wal)
    }

    /// Returns the number of entries in the WAL.
    pub fn len(&self) -> usize {
        let guard = self.state.read().unwrap();
        guard.entries.len()
    }

    /// Returns if the WAL is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends a new entry to the WAL.
    pub fn append(&self, entry: T) {
        let mut guard = self.state.write().unwrap();
        guard.entries.push(entry);
    }

    /// Clears the WAL of all entries.
    pub fn clear(&self) -> Result<(), SecureWALError<G::Error, C::Error, IO::Error>>
    where
        G: Ivg,
        C: StatefulCrypter,
    {
        let mut guard = self.state.write().unwrap();

        // Clear in-memory state.
        guard.entries.clear();
        guard.cursor = 0;

        // Clear on-disk state.
        let mut fs = self.fs.lock().unwrap();
        let mut file = File::create(&mut fs, &self.path)?;
        file.set_len(0)?;

        Ok(())
    }

    /// Persists all WAL entries.
    /// Each WAL entry is laid out as: [len, iv, serialization, checksum].
    pub fn persist(&self, key: Key<N>) -> Result<(), SecureWALError<G::Error, C::Error, IO::Error>>
    where
        T: Serialize + Sized,
        G: Ivg,
        C: StatefulCrypter,
    {
        let mut guard = self.state.write().unwrap();

        // Split the borrows.
        let state = guard.deref_mut();
        let cursor = &mut state.cursor;
        let entries = &state.entries;
        let ivg = &mut state.ivg;

        // Append to the on-disk WAL.
        let fs = self.fs.lock().unwrap();
        let mut file = File::options(&fs)
            .write(true)
            .append(true)
            .create(true)
            .open(&self.path)?;

        for entry in entries.iter().skip(*cursor) {
            let mut iv = vec![0; C::iv_length()];
            ivg.gen(&mut iv).map_err(SecureWALError::IV)?;

            let mut ser = bincode::serialize(entry)?;
            self.crypter
                .encrypt(&key, &iv, &mut ser)
                .map_err(SecureWALError::Crypt)?;

            let len = (ser.len() as u64).to_le_bytes();
            let checksum = self.crc.checksum(&ser).to_le_bytes();

            file.write_all(&len)?;
            file.write_all(&iv)?;
            file.write_all(&ser)?;
            file.write_all(&checksum)?;
            file.sync_data()?;

            *cursor += 1;
        }

        Ok(())
    }

    /// Loads all persisted WAL entries, ignoring corrupted ones.
    /// Each WAL entry is laid out as: [len, iv, serialization, checksum].
    fn load(&self, key: Key<N>) -> Result<(), SecureWALError<G::Error, C::Error, IO::Error>>
    where
        T: for<'de> Deserialize<'de>,
        G: Ivg,
        C: StatefulCrypter,
    {
        let mut guard = self.state.write().unwrap();

        // Split the borrows.
        let state = guard.deref_mut();
        let cursor = &mut state.cursor;
        let entries = &mut state.entries;

        let fs = self.fs.lock().unwrap();
        // Open up the on-disk WAL.
        let mut file = File::options(&fs)
            .read(true)
            .write(true)
            .create(true)
            .open(&self.path)?;

        let mut iv = vec![0; C::iv_length()];
        let mut len = [0; mem::size_of::<u64>()];
        let mut checksum = [0; mem::size_of::<u64>()];
        let mut wal_len = 0;

        loop {
            // Read the length of the entry.
            let len = if file.read_exact(&mut len).is_ok() {
                u64::from_le_bytes(len)
            } else {
                break;
            };

            // Read the IV used to encrypt the entry.
            if file.read_exact(&mut iv).is_err() {
                break;
            }

            // Read the encrypted serialized entry.
            let mut ser = vec![0; len as usize];
            if file.read_exact(&mut ser).is_err() {
                break;
            }

            // Read the checksum of the encrypted serialized entry.
            let checksum = if file.read_exact(&mut checksum).is_ok() {
                u64::from_le_bytes(checksum)
            } else {
                break;
            };

            // Validate checksum.
            if self.crc.checksum(&ser) != checksum {
                break;
            }

            // Decrypt the encrypted serialized entry.
            self.crypter
                .decrypt(&key, &iv, &mut ser)
                .map_err(SecureWALError::Crypt)?;

            // Add the deserialized entry and increment count of persisted entries.
            entries.push(bincode::deserialize(&ser)?);
            *cursor += 1;

            // Update the length (in bytes) of the WAL.
            wal_len += mem::size_of_val(&len) + iv.len() + ser.len() + mem::size_of_val(&checksum);
        }

        // Truncate the log so it doesn't contain any corrupted entries.
        file.set_len(wal_len as u64)?;

        Ok(())
    }

    /// Returns an iterator over the in-memory entries of the WAL.
    pub fn iter(&self) -> Iter<T, G, N>
    where
        T: Clone,
    {
        Iter {
            guard: self.state.read().unwrap(),
            index: 0,
        }
    }

    /// Returns an iterator over the persisted entries of the WAL.
    pub fn iter_persisted(
        &self,
        key: Key<N>,
        fs: &'a mut FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>,
    ) -> Result<IterPersisted<IO, T, G, C, N>, io::Error>
    where
        T: for<'de> Deserialize<'de>,
    {
        Ok(IterPersisted {
            _guard: self.state.read().unwrap(),
            file: File::open(fs, &self.path)?,
            key,
            crc: &self.crc,
            crypter: &self.crypter,
            _pd: PhantomData,
        })
    }
}

impl<'a, IO, T, G, C, const N: usize> IntoIterator for &'a SecureWAL<'a, IO, T, G, C, N>
where
    T: Clone,
    IO: ReadWriteSeek + 'static,
    std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
{
    type IntoIter = Iter<'a, T, G, N>;
    type Item = T;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct Iter<'a, T, G, const N: usize> {
    guard: RwLockReadGuard<'a, State<T, G, N>>,
    index: usize,
}

impl<T, G, const N: usize> Iterator for Iter<'_, T, G, N>
where
    T: Clone,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.index += 1;
        self.guard
            .entries
            .get(self.index - 1)
            .map(|entry| entry.clone())
    }
}

pub struct IterPersisted<'a, IO: ReadWriteSeek, T, G, C, const N: usize> {
    _guard: RwLockReadGuard<'a, State<T, G, N>>,
    file: File<'a, IO>,
    key: Key<N>,
    crc: &'a Crc<u64>,
    crypter: &'a C,
    _pd: PhantomData<T>,
}

impl<IO, T, G, C, const N: usize> Iterator for IterPersisted<'_, IO, T, G, C, N>
where
    T: for<'de> Deserialize<'de> + std::fmt::Debug,
    C: StatefulCrypter,
    IO: ReadWriteSeek,
    std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let mut iv = vec![0; C::iv_length()];
        let mut len = [0; mem::size_of::<u64>()];
        let mut checksum = [0; mem::size_of::<u64>()];

        let len = self
            .file
            .read_exact(&mut len)
            .and_then(|_| Ok(u64::from_le_bytes(len)))
            .ok()?;

        self.file.read_exact(&mut iv).ok()?;

        let mut ser = vec![0; len as usize];
        self.file.read_exact(&mut ser).ok()?;

        let checksum = self
            .file
            .read_exact(&mut checksum)
            .and_then(|_| Ok(u64::from_le_bytes(checksum)))
            .ok()?;

        if self.crc.checksum(&ser) != checksum {
            return None;
        }

        self.crypter.decrypt(&self.key, &iv, &mut ser).ok()?;

        bincode::deserialize(&ser).ok()
    }
}

// #[cfg(test)]
// mod tests {
//     use std::{collections::HashSet, fs, sync::Arc, thread};

//     use crate::crypter::{aes::Aes256Ctr, ivs::SequentialIvg};

//     use super::*;

//     #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
//     struct LogEntry {
//         id: usize,
//     }

//     const NUM_THREADS: usize = 128;
//     const KEY_SIZE: usize = 32;

//     #[test]
//     fn it_works() {
//         let wal = Arc::new(
//             SecureWAL::<LogEntry, SequentialIvg, Aes256Ctr, KEY_SIZE>::open(
//                 "/tmp/kms_test_secure_wal_it_works.log",
//                 [0; KEY_SIZE],
//             )
//             .unwrap(),
//         );

//         let handles: Vec<_> = (0..NUM_THREADS)
//             .map(|n| {
//                 thread::spawn({
//                     let wal = wal.clone();
//                     move || {
//                         wal.append(LogEntry { id: n });
//                     }
//                 })
//             })
//             .collect();

//         for handle in handles {
//             handle.join().unwrap();
//         }

//         let ids: HashSet<_> = HashSet::from_iter(wal.iter().map(|entry| entry.id));
//         assert_eq!(ids.len(), NUM_THREADS);
//     }

//     #[test]
//     fn reload() {
//         let _ = fs::remove_file("/tmp/kms_test_secure_wal_reload.log");

//         let key = [0; KEY_SIZE];

//         let wal = Arc::new(
//             SecureWAL::<LogEntry, SequentialIvg, Aes256Ctr, KEY_SIZE>::open(
//                 "/tmp/kms_test_secure_wal_reload.log",
//                 key,
//             )
//             .unwrap(),
//         );

//         let handles: Vec<_> = (0..4)
//             .map(|n| {
//                 thread::spawn({
//                     let wal = wal.clone();
//                     move || {
//                         wal.append(LogEntry { id: n });
//                     }
//                 })
//             })
//             .collect();

//         for handle in handles {
//             handle.join().unwrap();
//         }

//         wal.persist(key).unwrap();

//         let wal = Arc::new(
//             SecureWAL::<LogEntry, SequentialIvg, Aes256Ctr, KEY_SIZE>::open(
//                 "/tmp/kms_test_secure_wal_reload.log",
//                 key,
//             )
//             .unwrap(),
//         );

//         let expected: HashSet<_> = (0..4).map(|n| LogEntry { id: n }).collect();
//         let actual: HashSet<_> = wal.iter_persisted(key).unwrap().collect();
//         assert_eq!(expected, actual);
//     }
// }
