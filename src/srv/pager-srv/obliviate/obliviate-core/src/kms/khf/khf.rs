use std::{
    cmp::Ordering,
    collections::HashSet,
    fmt::{self, Debug},
    marker::PhantomData,
};

use fatfs::{DefaultTimeProvider, FileSystem, IoBase, LossyOemCpConverter, ReadWriteSeek};
use itertools::Itertools;
use lru_mem::{HeapSize, LruCache};
use serde::{Deserialize, Serialize};

use super::{error::Error, node::Node, topology::Topology, Pos};
use crate::{
    consts::KEY_CACHE_LIMIT,
    crypter::{aes::Aes256Ctr, ivs::SequentialIvg, Ivg, StatefulCrypter},
    hasher::Hasher,
    io::{crypt::OneshotCryptIo, stdio::StdIo, Read, Write},
    key::{Key, KeyGenerator},
    kms::{
        InstrumentedKeyManagementScheme, KeyManagementScheme, PersistableKeyManagementScheme,
        StableKeyManagementScheme, StableLogEntry,
    },
    syncfile::File,
    wal::SecureWAL,
};

/// The default level for roots created when mutating a `Khf`.
pub const DEFAULT_ROOT_LEVEL: u64 = 1;
/// The default fanout list for a `Khf`.
pub const DEFAULT_FANOUTS: &[u64] = &[4, 4, 4, 4];

/// A list of different mechanisms, or ways, to consolidate a `Khf`.
pub enum Consolidation {
    /// Consolidate all keys to a single root.
    All,
    /// Consolidate all keys to subroots of a specified level.
    AllLeveled { level: u64 },
    /// Consolidate a range of keys with L1 subroots.
    Ranged { start: u64, end: u64 },
    /// Consolidate a range of keys with subroots of a specified level.
    RangedLeveled { level: u64, start: u64, end: u64 },
}

/// A keyed hash forest (`Khf`) is a data structure for secure key management built around keyed
/// hash trees (`Kht`s). As a secure key management scheme, a `Khf` is not only capable of deriving
/// keys, but also updating keys such that they cannot be rederived post-update. Updating a key is
/// synonymous to revoking a key.
///
/// ## Generics:
/// - R: [Rng](rand::Rng) - for generating new roots
/// - G: [IV Generator](Ivg) - used for persistence by the Crypter (C)
/// - C: [Crypter](crate::crypter::Crypter) - to encrypt and decrypt files
/// - H: [Hasher] - to derive the full tree from the root node.
/// - const N: [usize] - Number of bytes in a key
#[derive(Deserialize, Serialize)]
pub struct Khf<R, G, C, H, const N: usize> {
    // The topology of a `Khf`.
    pub(crate) topology: Topology,
    // The number of keys covered at the last update.
    pub(crate) keys: u64,
    // The number of keys covered currently.
    pub(crate) in_flight_keys: u64,
    // The list of roots.
    #[serde(bound(serialize = "Node<H, N>: Serialize"))]
    #[serde(bound(deserialize = "Node<H, N>: Deserialize<'de>"))]
    pub(crate) roots: Vec<Node<H, N>>,
    // Root that appended keys are derived from.
    // TODO: Add this to the end of the root list.
    #[serde(bound(serialize = "Node<H, N>: Serialize"))]
    #[serde(bound(deserialize = "Node<H, N>: Deserialize<'de>"))]
    pub(crate) spanning_root: Node<H, N>,
    // Holds computed subroots during an epoch.
    #[serde(skip)]
    #[serde(default = "key_cache_default")]
    pub(crate) write_cache: LruCache<Pos, Key<N>>,
    #[serde(skip)]
    #[serde(default = "key_cache_default")]
    pub(crate) read_cache: LruCache<Pos, Key<N>>,
    // Whether or not the `Khf` completely fragments on update.
    pub(crate) fragmented: bool,
    // Used for generating new roots.
    #[serde(skip)]
    pub(crate) rng: R,
    // Used for persistence.
    #[serde(skip)]
    pub(crate) ivg: G,
    // Used for persistence.
    #[serde(skip)]
    pub(crate) crypter: C,
}

impl<R, G, C, H, const N: usize> Debug for Khf<R, G, C, H, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.topology.descendants(0);
        f.debug_struct("Khf")
            .field("topology", &self.topology)
            .field("keys", &self.keys)
            .field("in_flight_keys", &self.in_flight_keys)
            .field("roots", &self.roots)
            .field("spanning_root", &self.spanning_root)
            .field("write_cache", &self.write_cache)
            .field("read_cache", &self.read_cache)
            .field("fragmented", &self.fragmented)
            .finish()
    }
}

fn key_cache_default<const N: usize>() -> LruCache<Pos, Key<N>> {
    LruCache::new(KEY_CACHE_LIMIT)
}

// This is just an approximation.
fn key_cache_heapsize<const N: usize>(cache: &LruCache<Pos, Key<N>>) -> usize {
    cache.len() * (std::mem::size_of::<Pos>() + std::mem::size_of::<Key<N>>())
}

impl<R, G, C, H, const N: usize> HeapSize for Khf<R, G, C, H, N> {
    fn heap_size(&self) -> usize {
        self.topology.heap_size()
            + self.keys.heap_size()
            + self.in_flight_keys.heap_size()
            + self.roots.heap_size()
            + self.spanning_root.heap_size()
            + key_cache_heapsize(&self.write_cache)
            + key_cache_heapsize(&self.read_cache)
    }
}

pub struct KhfBuilder<R, G, C, H, const N: usize> {
    fanouts: Vec<u64>,
    fragmented: bool,
    _pd: PhantomData<(R, G, C, H)>,
}

impl<R, G, C, H, const N: usize> KhfBuilder<R, G, C, H, N> {
    pub fn new() -> Self {
        Self {
            fanouts: DEFAULT_FANOUTS.to_vec(),
            fragmented: false,
            _pd: PhantomData,
        }
    }

    pub fn fanouts(&mut self, fanouts: &[u64]) -> &mut Self {
        self.fanouts = fanouts.to_vec();
        self
    }

    pub fn fragmented(&mut self, fragmented: bool) -> &mut Self {
        self.fragmented = fragmented;
        self
    }

    pub fn with_rng(&mut self) -> Khf<R, G, C, H, N>
    where
        R: KeyGenerator<N> + Default,
        G: Default,
        C: Default,
    {
        let mut rng = R::default();
        let root_key = rng.gen_key();
        let spanning_root_key = rng.gen_key();
        self.with_keys(root_key, spanning_root_key)
    }

    pub fn with_keys(&mut self, root_key: Key<N>, spanning_root_key: Key<N>) -> Khf<R, G, C, H, N>
    where
        R: Default,
        G: Default,
        C: Default,
    {
        Khf {
            topology: Topology::new(&self.fanouts),
            keys: 0,
            in_flight_keys: 0,
            roots: vec![Node::new(root_key)],
            spanning_root: Node::new(spanning_root_key),
            write_cache: key_cache_default(),
            read_cache: key_cache_default(),
            fragmented: self.fragmented,
            rng: R::default(),
            ivg: G::default(),
            crypter: C::default(),
        }
    }
}

impl<R, G, C, H, const N: usize> Khf<R, G, C, H, N> {
    /// Constructs a new `Khf` with a default RNG and fanout list.
    pub fn new() -> Self
    where
        R: KeyGenerator<N> + Default,
        G: Default,
        C: Default,
    {
        Self::options().with_rng()
    }

    /// Constructs a new `Khf` with specified keys and the default fanout list.
    pub fn with_keys(root_key: Key<N>, spanning_root_key: Key<N>) -> Self
    where
        R: Default,
        G: Default,
        C: Default,
    {
        Self::options().with_keys(root_key, spanning_root_key)
    }

    /// Constructs a new `Khf` with a specified fanout list.
    pub fn with_fanouts(fanouts: &[u64]) -> Self
    where
        R: KeyGenerator<N> + Default,
        G: Default,
        C: Default,
    {
        Self::options().fanouts(fanouts).with_rng()
    }

    pub fn options() -> KhfBuilder<R, G, C, H, N> {
        KhfBuilder::new()
    }

    /// Returns the number of keys covered.
    pub fn num_keys(&self) -> u64 {
        self.in_flight_keys
    }

    /// Returns the number of roots in the `Khf`'s root list.
    pub fn num_roots(&self) -> u64 {
        self.roots.len() as u64
    }

    /// Returns `true` if the `Khf` is consolidated.
    pub fn is_consolidated(&self) -> bool {
        self.roots.len() == 1 && self.roots[0].pos == (0, 0)
    }

    /// Consolidates the `Khf` and returns the affected keys.
    pub fn consolidate(&mut self, mechanism: Consolidation) -> Vec<u64>
    where
        R: KeyGenerator<N>,
        H: Hasher<N>,
    {
        let root = Node::with_rng(&mut self.rng);
        match mechanism {
            Consolidation::All => self.consolidate_all(root),
            Consolidation::AllLeveled { level } => self.consolidate_all_leveled(level, root),
            Consolidation::Ranged { start, end } => self.consolidate_ranged(start, end, root),
            Consolidation::RangedLeveled { level, start, end } => {
                self.consolidate_ranged_leveled(level, start, end, root)
            }
        }
    }

    // Consolidates back into a single root.
    pub fn consolidate_all(&mut self, root: Node<H, N>) -> Vec<u64>
    where
        H: Hasher<N>,
    {
        self.consolidate_all_leveled(0, root)
    }

    // Consolidates to roots of a certain level.
    pub fn consolidate_all_leveled(&mut self, level: u64, root: Node<H, N>) -> Vec<u64>
    where
        H: Hasher<N>,
    {
        let affected = (0..self.keys).into_iter().collect();
        self.update_keys(level, 0, self.in_flight_keys, root);
        affected
    }

    // Consolidates the roots for a range of keys.
    pub fn consolidate_ranged(&mut self, start: u64, end: u64, root: Node<H, N>) -> Vec<u64>
    where
        H: Hasher<N>,
    {
        // TODO: check if this is within bounds of in-flight keys.
        self.consolidate_ranged_leveled(DEFAULT_ROOT_LEVEL, start, end, root)
    }

    // Consolidates the roots for a range of keys to roots of a certain level.
    pub fn consolidate_ranged_leveled(
        &mut self,
        level: u64,
        start: u64,
        end: u64,
        root: Node<H, N>,
    ) -> Vec<u64>
    where
        H: Hasher<N>,
    {
        let affected = (start..end).into_iter().collect();
        self.update_keys(level, start, end, root);
        affected
    }

    pub fn derive_inner(&mut self, key_id: u64) -> Option<Key<N>>
    where
        H: Hasher<N>,
    {
        let pos = self.topology.leaf_position(key_id);

        // We don't cover this key yet. Note that we don't check if the KHF if
        // consolidated in order to force the user to use derive_mut to first
        // inform the KHF of a new key. If the forest was consolidated, we
        // technically are able to derive the key for any block, but would be
        // unable to log the number of in-flight keys.
        if key_id >= self.in_flight_keys {
            return None;
        }

        // We might have derived this key before.
        if let Some(key) = self.read_cache.get(&pos) {
            return Some(*key);
        }

        // Get the root that covers the key.
        let root = match self.roots.binary_search_by(|root| {
            if self.topology.is_ancestor(root.pos, pos) {
                Ordering::Equal
            } else if self.topology.end(root.pos) <= self.topology.start(pos) {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }) {
            Ok(index) => &self.roots[index],
            Err(_) => &self.spanning_root,
        };

        Some(root.derive_and_cache(&self.topology, pos, &mut self.read_cache))
    }

    // This function exists as a way to mark a key as updated without going
    // through `derive_mut_inner()` (this is mainly for the system KHF).
    pub fn mark_key_inner(&mut self, key_id: u64) {
        self.in_flight_keys = self.in_flight_keys.max(key_id + 1);
    }

    pub fn derive_mut_inner(&mut self, key_id: u64) -> Key<N>
    where
        H: Hasher<N>,
    {
        let pos = self.topology.leaf_position(key_id);

        // We might have derived this key before.
        if let Some(key) = self.write_cache.get(&pos) {
            return *key;
        }

        // Make sure we cover this key.
        self.mark_key_inner(key_id);

        // Get the root that covers the key.
        let root = match self.roots.binary_search_by(|root| {
            if self.topology.is_ancestor(root.pos, pos) {
                Ordering::Equal
            } else if self.topology.end(root.pos) <= self.topology.start(pos) {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }) {
            Ok(index) => &self.roots[index],
            Err(_) => &self.spanning_root,
        };

        root.derive_mut_and_cache(
            &self.topology,
            pos,
            &mut self.write_cache,
            &mut self.read_cache,
        )
    }

    pub fn ranged_derive_inner(
        &mut self,
        start_key_id: u64,
        end_key_id: u64,
    ) -> RangedDeriveIter<'_, R, G, C, H, N>
    where
        H: Hasher<N>,
    {
        RangedDeriveIter::new(self, start_key_id, end_key_id)
    }

    pub fn ranged_derive_mut_inner(
        &mut self,
        start_key_id: u64,
        end_key_id: u64,
    ) -> RangedDeriveMutIter<'_, R, G, C, H, N>
    where
        H: Hasher<N>,
    {
        RangedDeriveMutIter::new(self, start_key_id, end_key_id)
    }

    pub fn delete_key_inner(&mut self, key_id: u64) -> bool {
        // We can only delete off the end (i.e., can only truncate).
        if key_id + 1 == self.in_flight_keys {
            self.in_flight_keys = key_id;
            true
        } else {
            false
        }
    }

    /// Truncates to a desired number of keys.
    pub fn truncate_keys(&mut self, keys: u64)
    where
        H: Hasher<N>,
    {
        if self.is_consolidated() {
            // If we're consolidated, we'll just replace roots up to the desired
            // number of keys with the top-level root.
            self.roots = self.roots[0].coverage(&self.topology, DEFAULT_ROOT_LEVEL, 0, keys)
        } else {
            // Otherwise, we need to find the root that covers the last key and
            // truncate from there.
            let index = self
                .roots
                .iter()
                .position(|root| self.topology.end(root.pos) > keys)
                .unwrap();

            let start = self.topology.start(self.roots[index].pos);
            let root = self.roots.drain(index..).next().unwrap();

            self.roots
                .append(&mut root.coverage(&self.topology, DEFAULT_ROOT_LEVEL, start, keys));
        }
    }

    pub fn updated_key_id_ranges(&self, updated_keys: &HashSet<u64>) -> Vec<(u64, u64)> {
        if updated_keys.is_empty() {
            return Vec::new();
        }

        let mut ranges = Vec::new();
        let mut first = true;
        let mut start = 0;
        let mut prev = 0;
        let mut leaves = 1;

        for leaf in itertools::sorted(updated_keys.iter()) {
            if first {
                first = false;
                start = *leaf;
            } else if *leaf == prev + 1 {
                leaves += 1;
            } else {
                ranges.push((start, start + leaves));
                leaves = 1;
                start = *leaf;
            }
            prev = *leaf;
        }

        ranges.push((start, start + leaves));
        ranges
    }

    /// Updates a range of keys with subroots derived from a given root.
    pub fn update_keys(&mut self, level: u64, start: u64, end: u64, root: Node<H, N>)
    where
        H: Hasher<N>,
    {
        // Level 0 means consolidating to a single root.
        if level == 0 {
            self.roots = vec![root];
            return;
        }

        // Fragment the forest to cover all the keys.
        if self.is_consolidated() {
            self.roots =
                self.roots[0].coverage(&self.topology, level, 0, self.in_flight_keys.max(end));
        }

        // We need to create a new set of roots and store updated roots.
        let mut roots = Vec::new();
        let mut updated = Vec::new();

        // Find the first root affected by the update.
        let update_start = self
            .roots
            .iter()
            .position(|root| start < self.topology.end(root.pos))
            .unwrap_or(self.roots.len() - 1);
        let update_root = &self.roots[update_start];
        if self.topology.start(update_root.pos) != start {
            updated.append(&mut update_root.coverage(
                &self.topology,
                level,
                self.topology.start(update_root.pos),
                start,
            ));
        }

        // Save roots before the first root affected by the update.
        roots.extend(&mut self.roots.drain(..update_start));

        // Add replacement roots derived from the given root.
        updated.append(&mut root.coverage(&self.topology, level, start, end));

        // Find the last root affected by the update.
        let mut update_end = self.roots.len();
        if end < self.topology.end(self.roots[self.roots.len() - 1].pos) {
            update_end = self
                .roots
                .iter()
                .position(|root| end <= self.topology.end(root.pos))
                .unwrap_or(self.roots.len())
                + 1;
            let update_root = &self.roots[update_end - 1];
            if self.topology.end(update_root.pos) != end {
                updated.append(&mut update_root.coverage(
                    &self.topology,
                    level,
                    end,
                    self.topology.end(update_root.pos),
                ));
            }
        }

        // Save the updated roots and add any remaining roots.
        roots.append(&mut updated);
        roots.extend(&mut self.roots.drain(update_end..));
        self.roots = roots;
    }

    pub fn update_inner(&mut self, updated_keys: &HashSet<u64>) -> Vec<(u64, Key<N>)>
    where
        R: KeyGenerator<N>,
        H: Hasher<N>,
    {
        if self.fragmented {
            return self.fragmented_update_inner(updated_keys);
        }

        // Derive the keys for each of the updated key IDs prior to update.
        let res = updated_keys
            .clone()
            .into_iter()
            .map(|key_id| (key_id, self.derive_inner(key_id).unwrap()))
            .collect_vec();

        // This root provides the new subroots to cover key updates.
        let updating_root = Node::with_rng(&mut self.rng);
        match (self.in_flight_keys, self.keys) {
            (0, _) => {
                // We've deleted every key, so we can just consolidate the forest.
                self.consolidate_all(updating_root);
            }
            (in_flight_keys, keys)
                if in_flight_keys >= keys && res.len() as u64 >= in_flight_keys =>
            {
                // We've added keys and also updated every key (including the
                // appended ones), so we can just consolidate the forest.
                self.consolidate_all(updating_root);
            }
            (in_flight_keys, keys) if in_flight_keys >= keys => {
                // We've added keys but haven't updated every key. First add in
                // the appended keys, then apply the updates.
                self.update_keys(DEFAULT_ROOT_LEVEL, keys, in_flight_keys, self.spanning_root);

                for (start, end) in self.updated_key_id_ranges(&updated_keys) {
                    self.update_keys(DEFAULT_ROOT_LEVEL, start, end, updating_root);
                }
            }
            (in_flight_keys, _keys) if res.len() as u64 >= in_flight_keys => {
                // We've truncated keys and also updated each of the remaining
                // keys, so we can just consolidate the forest.
                self.consolidate_all(updating_root);
            }
            (in_flight_keys, _keys) => {
                // We've truncated keys but haven't updated every key. First
                // truncate to the intended length, then apply any updates to
                // keys that haven't been truncated.
                self.truncate_keys(in_flight_keys);

                for (start, end) in self.updated_key_id_ranges(&updated_keys) {
                    self.update_keys(DEFAULT_ROOT_LEVEL, start, end, updating_root);
                }
            }
        }

        // To complete the update, we update our new number of keys and generate
        // a new spanning root.
        self.keys = self.in_flight_keys;
        self.spanning_root = Node::with_rng(&mut self.rng);

        // Clear the caches.
        self.write_cache.clear();
        self.read_cache.clear();

        res
    }

    fn fragmented_update_inner(&mut self, updated_keys: &HashSet<u64>) -> Vec<(u64, Key<N>)>
    where
        R: KeyGenerator<N>,
        H: Hasher<N>,
    {
        // Derive the keys for each of the updated key IDs prior to update.
        let res = updated_keys
            .clone()
            .into_iter()
            .map(|key_id| (key_id, self.derive_inner(key_id).unwrap()))
            .collect_vec();

        // This root provides the new subroots to cover key updates.
        let updating_root = Node::with_rng(&mut self.rng);

        match (self.in_flight_keys, self.keys) {
            (0, _) => {
                // We've deleted every key, so we can just consolidate the // forest.
                self.consolidate_all_leveled(self.topology.height() - 1, updating_root);
            }
            (in_flight_keys, keys)
                if in_flight_keys >= keys && res.len() as u64 >= in_flight_keys =>
            {
                // We've added keys and also updated every key (including the
                // appended ones), so we can just consolidate the forest.
                self.consolidate_all_leveled(self.topology.height() - 1, updating_root);
            }
            (in_flight_keys, keys) if in_flight_keys >= keys => {
                // We've added keys but haven't updated every key. First add in
                // the appended keys, then apply the updates.
                self.update_keys(
                    self.topology.height() - 1,
                    keys,
                    in_flight_keys,
                    self.spanning_root,
                );

                for (start, end) in self.updated_key_id_ranges(&updated_keys) {
                    self.update_keys(self.topology.height() - 1, start, end, updating_root);
                }
            }
            (in_flight_keys, _keys) if res.len() as u64 >= in_flight_keys => {
                // We've truncated keys and also updated each of the remaining
                // keys, so we can just consolidate the forest.
                self.consolidate_all_leveled(self.topology.height() - 1, updating_root);
            }
            (in_flight_keys, _keys) => {
                // We've truncated keys but haven't updated every key. First
                // truncate to the intended length, then apply any updates to
                // keys that haven't been truncated.
                self.truncate_keys(in_flight_keys);

                for (start, end) in self.updated_key_id_ranges(&updated_keys) {
                    self.update_keys(self.topology.height() - 1, start, end, updating_root);
                }
            }
        }

        // To complete the update, we update our new number of keys and generate a
        // new spanning root.
        self.keys = self.in_flight_keys;
        self.spanning_root = Node::with_rng(&mut self.rng);

        // Clear the cache.
        self.write_cache.clear();
        self.read_cache.clear();

        res
    }
}

impl<R, G, C, H, const N: usize> Clone for Khf<R, G, C, H, N>
where
    R: Default,
    G: Default,
    C: Default,
{
    fn clone(&self) -> Self {
        Self {
            topology: self.topology.clone(),
            spanning_root: self.spanning_root,
            roots: self.roots.clone(),
            keys: self.keys,
            in_flight_keys: self.in_flight_keys,
            fragmented: self.fragmented,
            write_cache: self.write_cache.clone(),
            read_cache: self.read_cache.clone(),
            rng: R::default(),
            ivg: G::default(),
            crypter: C::default(),
        }
    }
}

impl<R, G, C, H, const N: usize> Default for Khf<R, G, C, H, N>
where
    R: KeyGenerator<N> + Default,
    G: Default,
    C: Default,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<R, G, C, H, const N: usize> fmt::Display for Khf<R, G, C, H, N>
where
    H: Hasher<N>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, root) in self.roots.iter().enumerate() {
            root.fmt(f, &self.topology)?;
            if i + 1 != self.roots.len() {
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

impl<R, G, C, H, const N: usize> KeyManagementScheme for Khf<R, G, C, H, N>
where
    G: Ivg + Default,
    C: StatefulCrypter + Default,
{
    type KeyId = u64;
    type LogEntry = StableLogEntry;
    type Error = Error<G::Error, C::Error>;
}

impl<R, G, C, H, const N: usize> StableKeyManagementScheme<G, C, N> for Khf<R, G, C, H, N>
where
    R: KeyGenerator<N> + Default,
    G: Ivg + Default,
    C: StatefulCrypter + Default,
    H: Hasher<N>,
{
    fn derive(&mut self, key_id: Self::KeyId) -> Result<Option<Key<N>>, Self::Error> {
        Ok(self.derive_inner(key_id))
    }

    fn ranged_derive(
        &mut self,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error> {
        Ok(self.ranged_derive_inner(start_key_id, end_key_id).collect())
    }

    fn derive_mut<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        key_id: Self::KeyId,
    ) -> Result<Key<N>, Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
    {
        wal.append(StableLogEntry::Update {
            id: 0,
            block: key_id,
        });
        Ok(self.derive_mut_inner(key_id))
    }

    fn ranged_derive_mut<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        start_key_id: Self::KeyId,
        end_key_id: Self::KeyId,
        _spec_bounds: Option<(Self::KeyId, Self::KeyId)>,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
    {
        wal.append(StableLogEntry::UpdateRange {
            id: 0,
            start_block: start_key_id,
            end_block: end_key_id,
        });
        Ok(self.ranged_derive_inner(start_key_id, end_key_id).collect())
    }

    fn delete<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
        key_id: Self::KeyId,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
    {
        if self.delete_key_inner(key_id) {
            // This means that we deleted a key through truncation.
            wal.append(StableLogEntry::Delete {
                id: 0,
                block: key_id,
            });
        } else {
            // This means that we're trying to delete a key in the middle of the
            // forest. We can achieve the same result by updating the key, which
            // is handled in the update procedure.
            wal.append(StableLogEntry::Update {
                id: 0,
                block: key_id,
            });
        }

        Ok(())
    }

    fn update<IO>(
        &mut self,
        wal: &SecureWAL<IO, Self::LogEntry, G, C, N>,
    ) -> Result<Vec<(Self::KeyId, Key<N>)>, Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
    {
        let mut updated_keys = HashSet::new();

        // Replay the log.
        for entry in wal.iter() {
            match entry {
                StableLogEntry::UpdateRange {
                    id: 0,
                    start_block: start_key_id,
                    end_block: end_key_id,
                } => {
                    for (key_id, _) in self.ranged_derive_inner(start_key_id, end_key_id) {
                        updated_keys.insert(key_id);
                    }
                }
                StableLogEntry::Update {
                    id: 0,
                    block: key_id,
                } => {
                    self.derive_mut_inner(key_id);
                    updated_keys.insert(key_id);
                }
                StableLogEntry::Delete {
                    id: 0,
                    block: key_id,
                } => {
                    self.delete_key_inner(key_id);
                    updated_keys.remove(&key_id);
                }
                _ => {
                    unreachable!()
                }
            }
        }
        Ok(self.update_inner(&updated_keys))
    }
}

impl<R, G, C, H, const N: usize> PersistableKeyManagementScheme<N> for Khf<R, G, C, H, N>
where
    R: KeyGenerator<N> + Default,
    G: Ivg + Default,
    C: StatefulCrypter + Default,
    H: Hasher<N>,
{
    fn persist<IO>(
        &mut self,
        root_key: Key<N>,
        path: &str,
        fs: &FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>,
    ) -> Result<(), Self::Error>
    where
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
    {
        let ser = bincode::serialize(self)?;

        let mut io = OneshotCryptIo::new(
            StdIo::new(
                File::options(&fs)
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(path)
                    // TODO: better error handling
                    .map_err(|e| Error::Io(e.into()))?,
            ),
            root_key,
            &mut self.ivg,
            &self.crypter,
        );

        Ok(io.write_all(&ser).map_err(|e| {
            println!("{}", e);
            Error::Persist
        })?)
        // Ok(())
    }

    fn load<IO>(
        root_key: Key<N>,
        path: &str,
        fs: &FileSystem<IO, DefaultTimeProvider, LossyOemCpConverter>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized,
        IO: ReadWriteSeek + 'static,
        std::io::Error: From<fatfs::Error<<IO as IoBase>::Error>>,
    {
        let mut ser = vec![];
        let mut ivg = SequentialIvg::default();
        let crypter = Aes256Ctr::default();
        let mut io = OneshotCryptIo::new(
            StdIo::new(
                File::options(&fs)
                    .read(true)
                    .write(true)
                    .create(false)
                    .open(path)
                    // TODO: better error handling
                    .map_err(|e| Error::Io(e.into()))?,
            ),
            root_key,
            &mut ivg,
            &crypter,
        );

        io.read_to_end(&mut ser).map_err(|_| Error::Load)?;

        Ok(bincode::deserialize::<Self>(&ser)?)
    }
}

enum DerivePhase<H, const N: usize> {
    Uncovered,
    RootList((Node<H, N>, usize)),
    SpanningRoot,
}

pub struct RangedDeriveIter<'a, R, G, C, H, const N: usize> {
    start_key_id: u64,
    end_key_id: u64,
    khf: &'a mut Khf<R, G, C, H, N>,
    phase: DerivePhase<H, N>,
    derivation_path: Vec<Node<H, N>>,
}

impl<'a, R, G, C, H, const N: usize> RangedDeriveIter<'a, R, G, C, H, N>
where
    H: Hasher<N>,
{
    fn new(khf: &'a mut Khf<R, G, C, H, N>, start_key_id: u64, end_key_id: u64) -> Self {
        let leaf_pos = khf.topology.leaf_position(start_key_id);

        let phase = if start_key_id >= khf.in_flight_keys {
            DerivePhase::Uncovered
        } else {
            match khf.roots.binary_search_by(|root| {
                if khf.topology.is_ancestor(root.pos, leaf_pos) {
                    Ordering::Equal
                } else if khf.topology.end(root.pos) <= khf.topology.start(leaf_pos) {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }) {
                Ok(index) => DerivePhase::RootList((khf.roots[index], index)),
                Err(_) => DerivePhase::SpanningRoot,
            }
        };

        let root = match phase {
            DerivePhase::Uncovered => None,
            DerivePhase::RootList((root, _)) => Some(root),
            DerivePhase::SpanningRoot => Some(khf.spanning_root),
        };

        let derivation_path = root
            .map(|root| {
                if root.pos == leaf_pos {
                    vec![]
                } else {
                    khf.topology
                        .parent_derivation_path::<H, N>(root.key, root.pos, leaf_pos)
                        .map(|(pos, key)| Node::with_pos(pos, key))
                        .collect()
                }
            })
            .unwrap_or(vec![]);

        Self {
            start_key_id,
            end_key_id,
            khf,
            phase,
            derivation_path,
        }
    }
}

impl<R, G, C, H, const N: usize> Iterator for RangedDeriveIter<'_, R, G, C, H, N>
where
    R: KeyGenerator<N> + Default,
    G: Ivg + Default,
    C: StatefulCrypter + Default,
    H: Hasher<N>,
{
    type Item = (u64, Key<N>);

    fn next(&mut self) -> Option<Self::Item> {
        if matches!(self.phase, DerivePhase::Uncovered)
            || self.start_key_id >= self.khf.num_keys()
            || self.start_key_id >= self.end_key_id
        {
            return None;
        }

        // When deriving a range of keys, we can be in two phases:
        //
        //  1) Deriving keys from roots in the root list
        //  2) Deriving keys from the spanning root
        //
        // With case (1), we need to make sure the derivation path actually
        // leads to the desired leaf key, which is carried out by popping the
        // path until we get to an ancestor. If the path is emptied, then the
        // current root doesn't cover the leaf key, which means we need to move
        // on to the next root in the root list, or fallback to the spanning
        // root. In either case, we need to set up the derivation path to the
        // ancestor of the current leaf key.
        //
        // With case (2), we again need to make sure the derivation path
        // actually leads to the desired leaf key, which is carried out by
        // popping the path until we get to an ancestor. It is impossible for
        // the path to be emptied in this case because the spanning root covers
        // effectively the whole key range. We then set up the derivation path
        // to the ancestor of the current leaf key.
        let start_key_id = self.start_key_id;
        let leaf_pos = self.khf.topology.leaf_position(self.start_key_id);
        self.start_key_id += 1;

        // Pop until we get to an ancestor, or until nothing's left.
        while self
            .derivation_path
            .last()
            .filter(|root| !self.khf.topology.is_ancestor(root.pos, leaf_pos))
            .is_some()
        {
            self.derivation_path.pop();
        }

        // Get the closest ancestor to the current leaf.
        let ancestor = match self.phase {
            DerivePhase::RootList((root, index)) => {
                // If there's nothing left, then the current root doesn't cover
                // the current leaf key. This means we need to move onto the
                // next root in the root list, or fallback to the spanning root
                // if we've exhausted the root list.
                if self.derivation_path.is_empty() {
                    if root.pos == leaf_pos {
                        // The current root is the leaf.
                        return Some((start_key_id, root.key));
                    } else if index + 1 >= self.khf.roots.len() {
                        // No more roots in the root list, fallback to spanning root.
                        self.phase = DerivePhase::SpanningRoot;
                        self.khf.spanning_root
                    } else {
                        // Move onto the new root in the root list.
                        let new_index = index + 1;
                        let new_root = self.khf.roots[new_index];
                        self.phase = DerivePhase::RootList((new_root, new_index));

                        // It's possible that this new root is the leaf.
                        if new_root.pos == leaf_pos {
                            return Some((start_key_id, new_root.key));
                        }

                        new_root
                    }
                } else {
                    *self.derivation_path.last().unwrap()
                }
            }
            DerivePhase::SpanningRoot => *self.derivation_path.last().unwrap(),
            DerivePhase::Uncovered => unreachable!(),
        };

        // Derive to the leaf.
        self.derivation_path.extend(
            self.khf
                .topology
                .derivation_path::<H, N>(ancestor.key, ancestor.pos, leaf_pos)
                .map(|(pos, key)| Node::with_pos(pos, key)),
        );

        // The leaf is last in the derivation path.
        self.derivation_path
            .last()
            .map(|root| (start_key_id, root.derive(&self.khf.topology, leaf_pos)))
    }
}

enum DeriveMutPhase<H, const N: usize> {
    RootList((Node<H, N>, usize)),
    SpanningRoot,
}

pub struct RangedDeriveMutIter<'a, R, G, C, H, const N: usize> {
    start_key_id: u64,
    end_key_id: u64,
    khf: &'a mut Khf<R, G, C, H, N>,
    phase: DeriveMutPhase<H, N>,
    derivation_path: Vec<Node<H, N>>,
}

impl<'a, R, G, C, H, const N: usize> RangedDeriveMutIter<'a, R, G, C, H, N>
where
    H: Hasher<N>,
{
    fn new(khf: &'a mut Khf<R, G, C, H, N>, start_key_id: u64, end_key_id: u64) -> Self {
        let leaf_pos = khf.topology.leaf_position(start_key_id);

        let phase = match khf.roots.binary_search_by(|root| {
            if khf.topology.is_ancestor(root.pos, leaf_pos) {
                Ordering::Equal
            } else if khf.topology.end(root.pos) <= khf.topology.start(leaf_pos) {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }) {
            Ok(index) => DeriveMutPhase::RootList((khf.roots[index], index)),
            Err(_) => DeriveMutPhase::SpanningRoot,
        };

        let root = match phase {
            DeriveMutPhase::RootList((root, _)) => root,
            DeriveMutPhase::SpanningRoot => khf.spanning_root,
        };

        let derivation_path = if root.pos == leaf_pos {
            vec![]
        } else {
            khf.topology
                .parent_derivation_path::<H, N>(root.key, root.pos, leaf_pos)
                .map(|(pos, key)| Node::with_pos(pos, key))
                .collect()
        };

        Self {
            start_key_id,
            end_key_id,
            khf,
            phase,
            derivation_path,
        }
    }
}

impl<R, G, C, H, const N: usize> Iterator for RangedDeriveMutIter<'_, R, G, C, H, N>
where
    R: KeyGenerator<N> + Default,
    G: Ivg + Default,
    C: StatefulCrypter + Default,
    H: Hasher<N>,
{
    type Item = (u64, Key<N>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.start_key_id >= self.end_key_id {
            return None;
        }

        // When deriving a range of keys, we can be in two phases:
        //
        //  1) Deriving keys from roots in the root list
        //  2) Deriving keys from the spanning root
        //
        // With case (1), we need to make sure the derivation path actually
        // leads to the desired leaf key, which is carried out by popping the
        // path until we get to an ancestor. If the path is emptied, then the
        // current root doesn't cover the leaf key, which means we need to move
        // on to the next root in the root list, or fallback to the spanning
        // root. In either case, we need to set up the derivation path to the
        // ancestor of the current leaf key.
        //
        // With case (2), we again need to make sure the derivation path
        // actually leads to the desired leaf key, which is carried out by
        // popping the path until we get to an ancestor. It is impossible for
        // the path to be emptied in this case because the spanning root covers
        // effectively the whole key range. We then set up the derivation path
        // to the ancestor of the current leaf key.
        let start_key_id = self.start_key_id;
        let leaf_pos = self.khf.topology.leaf_position(self.start_key_id);
        self.khf.mark_key_inner(self.start_key_id);
        self.start_key_id += 1;

        // Pop until we get to an ancestor, or until nothing's left.
        while self
            .derivation_path
            .last()
            .filter(|root| !self.khf.topology.is_ancestor(root.pos, leaf_pos))
            .is_some()
        {
            self.derivation_path.pop();
        }

        // Get the closest ancestor to the current leaf.
        let ancestor = match self.phase {
            DeriveMutPhase::RootList((root, index)) => {
                // If there's nothing left, then the current root doesn't cover
                // the current leaf key. This means we need to move onto the
                // next root in the root list, or fallback to the spanning root
                // if we've exhausted the root list.
                if self.derivation_path.is_empty() {
                    if root.pos == leaf_pos {
                        // The current root is the leaf.
                        return Some((start_key_id, root.key));
                    } else if index + 1 >= self.khf.roots.len() {
                        // No more roots in the root list, fallback to spanning root.
                        self.phase = DeriveMutPhase::SpanningRoot;
                        self.khf.spanning_root
                    } else {
                        // Move onto the new root in the root list.
                        let new_index = index + 1;
                        let new_root = self.khf.roots[new_index];
                        self.phase = DeriveMutPhase::RootList((new_root, new_index));

                        // It's possible that this new root is the leaf.
                        if new_root.pos == leaf_pos {
                            return Some((start_key_id, new_root.key));
                        }

                        new_root
                    }
                } else {
                    *self.derivation_path.last().unwrap()
                }
            }
            DeriveMutPhase::SpanningRoot => *self.derivation_path.last().unwrap(),
        };

        // Derive to the leaf.
        self.derivation_path.extend(
            self.khf
                .topology
                .derivation_path::<H, N>(ancestor.key, ancestor.pos, leaf_pos)
                .map(|(pos, key)| Node::with_pos(pos, key)),
        );

        // The leaf is last in the derivation path.
        self.derivation_path
            .last()
            .map(|root| (start_key_id, root.derive(&self.khf.topology, leaf_pos)))
    }
}

#[derive(Serialize)]
pub struct KhfStats {
    pub keys: u64,
    pub in_flight_keys: u64,
    pub num_roots: u64,
}

impl<R, G, C, H, const N: usize> InstrumentedKeyManagementScheme for Khf<R, G, C, H, N>
where
    G: Ivg + Default,
    C: StatefulCrypter + Default,
{
    type Stats = KhfStats;

    fn report_stats(&mut self) -> Result<Self::Stats, Self::Error> {
        Ok(KhfStats {
            keys: self.keys,
            in_flight_keys: self.in_flight_keys,
            num_roots: self.num_roots(),
        })
    }
}

// #[cfg(test)]
// mod tests {
//     use std::{
//         collections::{BTreeMap, HashMap},
//         fs,
//     };

//     use anyhow::Result;
//     use rand::rngs::ThreadRng;

//     use crate::hasher::sha3::{Sha3_256, SHA3_256_MD_SIZE};

//     use super::*;

//     const TEST_KEY: [u8; SHA3_256_MD_SIZE] = [0; SHA3_256_MD_SIZE];

//     struct KhfHarness {
//         khf: Khf<ThreadRng, SequentialIvg, Aes256Ctr, Sha3_256, SHA3_256_MD_SIZE>,
//         wal: SecureWAL<<Khf<ThreadRng, SequentialIvg, Aes256Ctr, Sha3_256, SHA3_256_MD_SIZE> as
// KeyManagementScheme>::LogEntry, SequentialIvg, Aes256Ctr, SHA3_256_MD_SIZE>,     }

//     impl KhfHarness {
//         fn wal_path(name: &str) -> String {
//             format!("/tmp/khf_test_{name}.log")
//         }

//         fn with_test_name(name: &str) -> Self {
//             let wal_path = Self::wal_path(name);

//             let _ = fs::remove_file(&wal_path);

//             Self {
//                 khf: Khf::default(),
//                 wal: SecureWAL::open(&wal_path, TEST_KEY).unwrap(),
//             }
//         }

//         fn with_test_name_and_fanouts(name: &str, fanouts: &[u64]) -> Self {
//             let wal_path = Self::wal_path(name);

//             let _ = fs::remove_file(&wal_path);

//             Self {
//                 khf: Khf::options().fanouts(fanouts).with_rng(),
//                 wal: SecureWAL::open(&wal_path, TEST_KEY).unwrap(),
//             }
//         }

//         fn with_test_name_and_fanouts_fragmented(name: &str, fanouts: &[u64]) -> Self {
//             let wal_path = Self::wal_path(name);

//             let _ = fs::remove_file(&wal_path);

//             Self {
//                 khf: Khf::options().fanouts(fanouts).fragmented(true).with_rng(),
//                 wal: SecureWAL::open(&wal_path, TEST_KEY).unwrap(),
//             }
//         }

//         fn with_test_name_and_fanouts_and_keys(
//             name: &str,
//             fanouts: &[u64],
//             root_key: Key<SHA3_256_MD_SIZE>,
//             spanning_root_key: Key<SHA3_256_MD_SIZE>,
//         ) -> Self {
//             let wal_path = Self::wal_path(name);

//             let _ = fs::remove_file(&wal_path);

//             Self {
//                 khf: Khf::options()
//                     .fanouts(fanouts)
//                     .with_keys(root_key, spanning_root_key),
//                 wal: SecureWAL::open(&wal_path, TEST_KEY).unwrap(),
//             }
//         }
//     }

//     #[test]
//     fn derive() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name("derive");

//         // We should not be able to derive any read keys yet.
//         assert!((0..8)
//             .filter_map(|key_id| khf.derive(key_id).ok())
//             .all(|res| res.is_none()));

//         // Derive the write keys.
//         let write_keys = (0..8)
//             .filter_map(|key_id| khf.derive_mut(&wal, key_id).ok())
//             .collect_vec();
//         assert_eq!(write_keys.len(), 8);

//         // Derive the read keys.
//         let read_keys = (0..8)
//             .filter_map(|key_id| khf.derive(key_id).ok())
//             .map(|key| key.unwrap())
//             .collect_vec();
//         assert_eq!(write_keys, read_keys);

//         Ok(())
//     }

//     #[test]
//     fn update() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name("update");

//         // First, derive some new keys.
//         let keys = (0..8)
//             .filter_map(|key_id| khf.derive_mut(&wal, key_id).ok())
//             .collect_vec();
//         assert_eq!(keys.len(), 8);

//         // Update the KHF, which should give us back the old keys.
//         let update = khf.update(&wal)?.into_iter().collect::<HashMap<_, _>>();
//         for (key_id, key) in keys.iter().enumerate() {
//             assert_eq!(update.get(&(key_id as u64)), Some(key));
//         }

//         // We shouldn't have the old keys anymore.
//         for (key_id, key) in keys.iter().enumerate() {
//             assert_ne!(khf.derive(key_id as u64)?.unwrap(), *key);
//         }

//         Ok(())
//     }

//     #[test]
//     fn delete() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name("delete");

//         // First, "derive" some new keys.
//         for key_id in 0..8 {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Apply the updates.
//         khf.update(&wal)?;

//         // Drop and re-open the WAL to get us to a clean slate.
//         drop(wal);
//         let wal_path = KhfHarness::wal_path("delete");
//         let _ = fs::remove_file(&wal_path);
//         let wal = SecureWAL::open(&wal_path, TEST_KEY)?;

//         // Get the keys.
//         let keys = (0..8)
//             .filter_map(|key_id| khf.derive(key_id).ok())
//             .map(|key| key.unwrap())
//             .collect_vec();
//         assert_eq!(keys.len(), 8);

//         // Truncate a key, then "delete" (becomes an update) a key.
//         khf.delete(&wal, 7)?;
//         khf.delete(&wal, 3)?;

//         // We should technically still be able to derive the old key since we
//         // haven't applied the updates yet.
//         assert_eq!(keys[3], khf.derive(3)?.unwrap());

//         // Apply the updates.
//         let update = khf.update(&wal)?.into_iter().collect_vec();
//         assert_eq!(update.len(), 1);

//         for (key_id, key) in keys.iter().enumerate() {
//             if key_id == 3 {
//                 assert_eq!(*key, update[0].1);
//                 assert_ne!(khf.derive(key_id as u64)?.unwrap(), update[0].1);
//             } else if key_id == 7 {
//                 assert!(khf.derive(key_id as u64)?.is_none());
//             } else {
//                 assert_eq!(khf.derive(key_id as u64)?.unwrap(), *key);
//             }
//         }

//         Ok(())
//     }

//     // This tests if ranged derivation yields the same result as iterative
//     // normal derivation.
//     #[test]
//     fn ranged_derive_mut() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name("ranged_derive_mut");

//         // Derive the keys normally.
//         let keys = (0..8)
//             .filter_map(|key_id| khf.derive_mut(&wal, key_id).map(|key| (key_id, key)).ok())
//             .collect_vec();
//         assert_eq!(keys.len(), 8);

//         // Derive the same keys using ranged derive.
//         // The derived keys should be exactly the same.
//         let ranged_keys: Vec<_> = khf.ranged_derive_mut_inner(0, 8).collect();
//         assert_eq!(keys, ranged_keys);

//         Ok(())
//     }

//     // This tests if ranged derivation yields the same result as iterative
//     // normal derivation when the KHF is completely fragmented.
//     #[test]
//     fn ranged_derive_mut_fragmented() -> Result<()> {
//         let KhfHarness { mut khf, wal } =
//             KhfHarness::with_test_name("ranged_derive_mut_fragmented");

//         // Derive the keys normally.
//         let num_test_keys = 5u64;
//         for key_id in 0..num_test_keys {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Initial commit to flush changes.
//         khf.update(&wal)?;
//         let mut keys: BTreeMap<_, _> = (0..num_test_keys)
//             .map(|key_id| (key_id, khf.derive(key_id).unwrap().unwrap()))
//             .collect();
//         assert_eq!(keys.len() as u64, num_test_keys);
//         wal.clear()?;

//         // Update the even keys.
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 2 == 0) {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Second commit to flush changes (this causes fragmentation).
//         // We should have new even keys from this update.
//         khf.update(&wal)?;
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 2 == 0) {
//             let key = khf.derive(key_id)?.unwrap();
//             keys.insert(key_id, key);
//         }
//         assert!(!khf.is_consolidated());
//         // eprintln!("{}", khf);

//         // Derive the same keys using ranged derive.
//         // The derived keys should be exactly the same.
//         let ranged_keys: BTreeMap<_, _> = khf.ranged_derive_mut_inner(0,
// num_test_keys).collect();         assert_eq!(keys, ranged_keys);

//         // for key_id in 0..num_test_keys {
//         //     let expected = hex::encode(keys.get(&key_id).unwrap());
//         //     let actual = hex::encode(ranged_keys.get(&key_id).unwrap());
//         //     eprintln!("key_id = {key_id}, expected = {expected}, actual = {actual}");
//         // }

//         Ok(())
//     }

//     // This tests if ranged derivation yields the same result as iterative
//     // normal derivation when the KHF is partially fragmented.
//     #[test]
//     fn ranged_derive_mut_semi_fragmented() -> Result<()> {
//         let KhfHarness { mut khf, wal } =
//             KhfHarness::with_test_name_and_fanouts("ranged_derive_mut_semi_fragmented", &[2, 2]);

//         // Derive the keys normally.
//         let num_test_keys = 12u64;
//         for key_id in 0..num_test_keys {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Initial commit to flush changes.
//         khf.update(&wal)?;
//         let mut keys: BTreeMap<_, _> = (0..num_test_keys)
//             .map(|key_id| (key_id, khf.derive(key_id).unwrap().unwrap()))
//             .collect();
//         assert_eq!(keys.len() as u64, num_test_keys);
//         wal.clear()?;

//         // Update every 3rd key.
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 3 == 0) {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Second commit to flush changes (this causes fragmentation).
//         // We should have new keys from this update.
//         khf.update(&wal)?;
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 3 == 0) {
//             let key = khf.derive(key_id)?.unwrap();
//             keys.insert(key_id, key);
//         }
//         assert!(!khf.is_consolidated());
//         // eprintln!("{}", khf);

//         // Derive the same keys using ranged derive.
//         // The derived keys should be exactly the same.
//         let ranged_keys: BTreeMap<_, _> = khf
//             .ranged_derive_mut_inner(0, num_test_keys)
//             // .inspect(|(key_id, key)| eprintln!("key_id = {key_id}, key = {}",
// hex::encode(key)))             .collect();
//         assert_eq!(keys, ranged_keys);

//         // for key_id in 0..num_test_keys {
//         //     let expected = hex::encode(keys.get(&key_id).unwrap());
//         //     let actual = hex::encode(ranged_keys.get(&key_id).unwrap());
//         //     eprintln!("key_id = {key_id}, expected = {expected}, actual = {actual}");
//         // }

//         Ok(())
//     }

//     // This tests if ranged derivation yields the same result as iterative
//     // normal derivation when the KHF is partially fragmented and must derive
//     // keys from the spanning root.
//     #[test]
//     fn ranged_derive_mut_semi_fragmented_and_beyond() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name_and_fanouts(
//             "ranged_derive_mut_semi_fragmented_and_beyond",
//             &[2, 2],
//         );

//         let num_test_keys = 12u64;
//         let max_test_keys = 24u64;

//         // Derive the keys normally.
//         for key_id in 0..num_test_keys {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Initial commit to flush changes.
//         khf.update(&wal)?;
//         let mut keys: BTreeMap<_, _> = (0..num_test_keys)
//             .map(|key_id| (key_id, khf.derive(key_id).unwrap().unwrap()))
//             .collect();
//         assert_eq!(keys.len() as u64, num_test_keys);
//         wal.clear()?;

//         // Update every 3rd key.
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 3 == 0) {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Second commit to flush changes (this causes fragmentation).
//         // We should have new keys from this update.
//         khf.update(&wal)?;
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 3 == 0) {
//             let key = khf.derive(key_id)?.unwrap();
//             keys.insert(key_id, key);
//         }
//         assert!(!khf.is_consolidated());
//         // eprintln!("{}", khf);

//         // Derive keys beyond the fragmented roots.
//         for key_id in num_test_keys..max_test_keys {
//             let key = khf.derive_mut(&wal, key_id)?;
//             keys.insert(key_id, key);
//         }

//         // Derive the same keys using ranged derive.
//         // The derived keys should be exactly the same.
//         let ranged_keys: BTreeMap<_, _> = khf
//             .ranged_derive_mut_inner(0, max_test_keys)
//             // .inspect(|(key_id, key)| eprintln!("key_id = {key_id}, key = {}",
// hex::encode(key)))             .collect();
//         assert_eq!(keys, ranged_keys);

//         // for key_id in 0..num_test_keys {
//         //     let expected = hex::encode(keys.get(&key_id).unwrap());
//         //     let actual = hex::encode(ranged_keys.get(&key_id).unwrap());
//         //     eprintln!("key_id = {key_id}, expected = {expected}, actual = {actual}");
//         // }

//         Ok(())
//     }

//     // This tests if ranged derivation yields the same result as iterative
//     // normal derivation when the KHF is fragmented and must derive keys from
//     // the spanning root. This uses the fragmented KHF setting.
//     #[test]
//     fn ranged_derive_mut_fragmented_and_beyond() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name_and_fanouts_fragmented(
//             "ranged_derive_mut_fragmented_and_beyond",
//             &[2, 2],
//         );

//         let num_test_keys = 12u64;
//         let max_test_keys = 24u64;

//         // Derive the keys normally.
//         for key_id in 0..num_test_keys {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Initial commit to flush changes.
//         khf.update(&wal)?;
//         let mut keys: BTreeMap<_, _> = (0..num_test_keys)
//             .map(|key_id| (key_id, khf.derive(key_id).unwrap().unwrap()))
//             .collect();
//         assert_eq!(keys.len() as u64, num_test_keys);
//         wal.clear()?;

//         // Update every 3rd key.
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 3 == 0) {
//             khf.derive_mut(&wal, key_id)?;
//         }

//         // Second commit to flush changes (this causes fragmentation).
//         // We should have new keys from this update.
//         khf.update(&wal)?;
//         for key_id in (0..num_test_keys).filter(|key_id| key_id % 3 == 0) {
//             let key = khf.derive(key_id)?.unwrap();
//             keys.insert(key_id, key);
//         }
//         assert!(!khf.is_consolidated());
//         // eprintln!("{}", khf);

//         // Derive keys beyond the fragmented roots.
//         for key_id in num_test_keys..max_test_keys {
//             let key = khf.derive_mut(&wal, key_id)?;
//             keys.insert(key_id, key);
//         }

//         // Derive the same keys using ranged derive.
//         // The derived keys should be exactly the same.
//         let ranged_keys: BTreeMap<_, _> = khf
//             .ranged_derive_mut_inner(0, max_test_keys)
//             // .inspect(|(key_id, key)| eprintln!("key_id = {key_id}, key = {}",
// hex::encode(key)))             .collect();
//         assert_eq!(keys, ranged_keys);

//         // for key_id in 0..num_test_keys {
//         //     let expected = hex::encode(keys.get(&key_id).unwrap());
//         //     let actual = hex::encode(ranged_keys.get(&key_id).unwrap());
//         //     eprintln!("key_id = {key_id}, expected = {expected}, actual = {actual}");
//         // }

//         Ok(())
//     }

//     #[test]
//     fn ranged_derive_mut_is_correct() -> Result<()> {
//         let mut rng = ThreadRng::default();
//         let root_key = rng.gen_key();
//         let spanning_root_key = rng.gen_key();

//         let KhfHarness {
//             khf: mut khf1,
//             wal: _wal1,
//         } = KhfHarness::with_test_name_and_fanouts_and_keys(
//             "ranged_derive_mut_is_correct",
//             &[2, 2],
//             root_key,
//             spanning_root_key,
//         );

//         let KhfHarness {
//             khf: mut khf2,
//             wal: _wal2,
//         } = KhfHarness::with_test_name_and_fanouts_and_keys(
//             "ranged_derive_mut_is_correct",
//             &[2, 2],
//             root_key,
//             spanning_root_key,
//         );

//         let num_keys = 24;

//         let keys1: Vec<_> = (0..num_keys)
//             .map(|block| khf1.derive_mut_inner(block))
//             .collect();
//         let keys2: Vec<_> = khf2
//             .ranged_derive_mut_inner(0, num_keys)
//             .map(|(_, key)| key)
//             .collect();

//         assert_eq!(keys1, keys2);

//         Ok(())
//     }

//     // This tests if ranged derivation is correctly bounded.
//     #[test]
//     fn ranged_derive_is_correct() -> Result<()> {
//         let KhfHarness { mut khf, wal } = KhfHarness::with_test_name("ranged_derive_is_correct");

//         // We shouldn't have any keys to begin with.
//         let keys = khf.ranged_derive(0, 1024)?;
//         assert!(keys.is_empty());

//         // We should now cover 1024 keys.
//         let derive_mut_keys = khf.ranged_derive_mut(&wal, 0, 1024, None)?;

//         // We should be bounded to 1024 keys.
//         let derive_keys = khf.ranged_derive(0, 2048)?;
//         assert_eq!(derive_mut_keys, derive_keys);

//         Ok(())
//     }
// }
