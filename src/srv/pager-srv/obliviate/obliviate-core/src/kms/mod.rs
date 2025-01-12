// pub mod ahashmap;
// pub mod hashmap;
pub mod khf;
// pub mod localizer;
// pub mod sbptree;

use std::{
    error::Error,
    fmt::{self, Debug},
    path::Path,
};

use fatfs::{DefaultTimeProvider, FileSystem, IoBase, LossyOemCpConverter, ReadWriteSeek};
use serde::{Deserialize, Serialize};

use crate::{
    key::{Key, KeyWrapper},
    wal::SecureWAL,
};

/// The base trait for crash-consistent key management schemes.
pub trait KeyManagementScheme {
    /// The type of a key ID.
    type KeyId: Copy;
    /// The type of a write-ahead-log entry.
    type LogEntry: for<'de> Deserialize<'de> + Serialize + Debug;
    /// The associated error for fallible operations.
    type Error: Error;
}

/// A persistable key management scheme is, as the name suggests, a key
/// management scheme that can be persisted to and loaded from persistent
/// storage. This is a separate extension trait since some key management
/// schemes are purely in-memory.
pub trait PersistableKeyManagementScheme<const N: usize>: KeyManagementScheme {
    /// Persists the KMS to the given path, encrypting it using the supplied
    /// root key. The KMS is responsible for setting up its own on-disk
    /// structure using the given path as the root. This function aligns the
    /// on-disk state of the KMS to match its in-memory state.
    fn persist<IO>(
        &mut self,
        root_key: Key<N>,
        path: &str,
        fs: &FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>;

    /// Loads a persisted KMS from the given path, decrypting it using the
    /// supplied root key. This function aligns the in-memory state of the KMS
    /// to match its on-disk state. This function should return a new instance
    /// of the KMS if the supplied path doesn't exist.
    fn load<IO>(
        root_key: Key<N>,
        path: &str,
        fs: &FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized,
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>;

    /// Sets the working directory of the KMS to the specified directory.
    /// This is only meaningful for a KMS that isn't contained entirely in-memory.
    fn rebase(&mut self, _path: impl AsRef<Path>) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A localized key management scheme is one that incorporates the notion of
/// grouping keys for locality. This trait allows such schemes to efficiently
/// delete keys that are grouped together under a logical object.
pub trait LocalizedKeyManagementScheme<G, C, L, const N: usize>: KeyManagementScheme {
    /// The type of an object identifier.
    type ObjectId: Copy;

    /// Deletes the object with the given object ID. A handle to an in-memory WAL is
    /// given for the KMS to record this change.
    fn delete_object<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        obj_id: Self::ObjectId,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek;

    /// Truncates the object with the given object ID to cover a specified number
    /// of keys. A handle to an in-memory WAL is given for the KMS to record
    /// this change. Unlike `ftruncate`, this is to be used strictly as a
    /// truncate operation and not a general resize operation.
    fn truncate_object<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        obj_id: Self::ObjectId,
        num_keys: u64,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek;
}

/// An instrumented key management scheme is one that is able to report
/// statistics about its internal state.
pub trait InstrumentedKeyManagementScheme: KeyManagementScheme {
    /// The type that holds the key management scheme's statistics.
    type Stats: Serialize;

    /// Returns the statistics about the key management scheme.
    fn report_stats(&mut self) -> Result<Self::Stats, Self::Error>;
}

/// A trait for key management schemes that allows for key derivation that
/// upholds the stable key management scheme principle. This means that the key
/// management scheme's key space stays the same between calls to `update`
/// (i.e., every call to `derive_mut` yields the same key between calls to
/// `update`).
pub trait StableKeyManagementScheme<G, C, const N: usize>:
    KeyManagementScheme<LogEntry = StableLogEntry>
{
    /// Derives the key with the given key ID. This function is designed to
    /// return keys that the KMS already covers internally inside `Some()`, and
    /// returns `None` if a key that isn't currently covered is requested.
    fn derive(&mut self, key_id: Self::KeyId) -> Result<Option<Key<N>>, Self::Error>;

    /// Computes a range of keys. This returns either the entire
    /// range of keys, or the keys up until the first one that isn't covered by
    /// the key management scheme.
    fn ranged_derive(
        &mut self,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>;

    /// Derives the key with the given key ID, adding the key internally if the
    /// key is not yet covered. A handle to an in-memory WAL is given for the
    /// KMS to log that the key ID was updated, as well as any changes to its
    /// internal state.
    fn derive_mut<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        key_id: Self::KeyId,
    ) -> Result<Key<N>, Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>;

    /// Derives a range of keys, marking each as updated. A handle to an
    /// in-memory WAL is given for the KMS to log that each key ID was updated,
    /// as well as any changes to its internal state. Any key updates that have
    /// been speculated on are not logged.
    fn ranged_derive_mut<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
        spec_bounds: Option<(Self::KeyId, Self::KeyId)>,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>;

    /// Deletes the key with the given key ID. A handle to an in-memory WAL is
    /// given for the KMS to record this change.
    fn delete<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        key_id: Self::KeyId,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>;

    /// Updates the internal state of the KMS to provide new keys for each
    /// updated block recorded in the in-memory WAL. The KMS returns a list of
    /// (key ID, key) tuples. Each tuple indicates the key ID that was updated
    /// and the old value of the key it mapped to.
    fn update<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>;
}

pub trait SpeculativeKeyManagementScheme<G, C, const N: usize>:
    StableKeyManagementScheme<G, C, N>
{
    /// Speculatively marks a range of keys as updated. A handle to an in-memory
    /// WAL is given for the KMS to log this update, as well as any changes to
    /// its internal state. Keys that have been speculated on don't require any
    /// further WAL entries during an epoch.
    fn speculate_range<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek;
}

pub trait AffineKeyManagementScheme<G, C, const N: usize>:
    KeyManagementScheme<LogEntry = StableLogEntry>
{
    /// Derives the key with the given key ID. This function is designed to
    /// return keys that the KMS already covers internally inside `Some()`, and
    /// returns `None` if a key that isn't currently covered is requested.
    fn derive_read_key(&mut self, key_id: Self::KeyId) -> Result<Option<Key<N>>, Self::Error>;

    /// Computes a range of keys. This returns either the entire
    /// range of keys, or the keys up until the first one that isn't covered by
    /// the key management scheme.
    fn derive_read_key_many(
        &mut self,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>;

    /// Derives the key with the given key ID, adding the key internally if the
    /// key is not yet covered. A handle to an in-memory WAL is given for the
    /// KMS to log that the key ID was updated, as well as any changes to its
    /// internal state.
    fn derive_write_key(&mut self, key_id: Self::KeyId) -> Result<Key<N>, Self::Error>;

    /// Derives a range of keys, marking each as updated. A handle to an
    /// in-memory WAL is given for the KMS to log that each key ID was updated,
    /// as well as any changes to its internal state. Any key updates that have
    /// been speculated on are not logged.
    fn derive_write_key_many(
        &mut self,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
        spec_bounds: Option<(Self::KeyId, Self::KeyId)>,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>;

    /// Deletes the key with the given key ID. A handle to an in-memory WAL is
    /// given for the KMS to record this change.
    fn delete<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        key_id: Self::KeyId,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek;

    /// Updates the internal state of the KMS to provide new keys for each
    /// updated block recorded in the in-memory WAL. The KMS returns a list of
    /// (key ID, key) tuples. Each tuple indicates the key ID that was updated
    /// and the old value of the key it mapped to.
    fn update(&mut self, entries: &[Self::LogEntry]) -> Result<(), Self::Error>;
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum StableLogEntry {
    UpdateRange {
        id: u64,
        start_block: u64,
        end_block: u64,
    },
    Update {
        id: u64,
        block: u64,
    },
    Delete {
        id: u64,
        block: u64,
    },
    DeleteObject {
        id: u64,
    },
}

/// A trait for key management schemes that allows for key derivation that
/// violates the stable key management scheme principle.  This means that the
/// key management scheme's key space changes between calls to `update` (i.e.,
/// every call to `derive_mut` yields a unique key). Notably, an unstable key
/// management scheme must also do full metadata/data journaling, and is given
/// the responsibility of logging modified key/data pairs.
pub trait UnstableKeyManagementScheme<G, C, const N: usize>:
    KeyManagementScheme<LogEntry = JournalEntry<N>>
{
    /// Derives the key with the given key ID. This function is designed to
    /// return keys that the KMS already covers internally inside `Some()`, and
    /// returns `None` if a key that isn't currently covered is requested.
    fn derive(&mut self, key_id: Self::KeyId) -> Result<Option<Key<N>>, Self::Error>;

    /// Derives a new key for the given key ID, adding the key internally if the
    /// key is not yet covered.
    fn derive_mut(&mut self, key_id: Self::KeyId) -> Result<Key<N>, Self::Error>;

    /// Syncs the internal state with the given journal entry. The journal entry
    /// is guaranteed to have been persisted prior to this call.
    fn sync(&mut self, entry: &Self::LogEntry) -> Result<(), Self::Error>;

    /// Deletes the key with the given key ID. A handle to an in-memory WAL is
    /// given for the KMS to record this change.
    fn delete<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        key_id: Self::KeyId,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek;

    /// Updates the internal state of the KMS to provide new keys for each
    /// updated block recorded in the in-memory WAL. The KMS returns a list of
    /// (key ID, key) tuples. Each tuple indicates the key ID that was updated
    /// and the old value of the key it mapped to.
    fn update<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
    ) -> Result<Vec<(Self::KeyId, (Key<N>, Vec<u8>))>, Self::Error>
    where
        IO: ReadWriteSeek;
}

#[derive(Deserialize, Serialize, Clone)]
pub enum JournalEntry<const N: usize> {
    Update {
        id: u64,
        block: u64,
        key: KeyWrapper<N>,
        data: Vec<u8>,
    },
    Delete {
        id: u64,
        block: u64,
    },
    DeleteObject {
        id: u64,
    },
}

impl<const N: usize> fmt::Debug for JournalEntry<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Update {
                id,
                block,
                key,
                data,
            } => f
                .debug_struct("Update")
                .field("id", id)
                .field("block", block)
                .field("key", key)
                .field("data", &hex::encode(data))
                .finish(),
            Self::Delete { id, block } => f
                .debug_struct("Delete")
                .field("id", id)
                .field("block", block)
                .finish(),
            Self::DeleteObject { id } => f.debug_struct("DeleteObject").field("id", id).finish(),
        }
    }
}
