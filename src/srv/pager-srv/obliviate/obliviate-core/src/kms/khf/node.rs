use std::{fmt, marker::PhantomData};

use lru_mem::{HeapSize, LruCache};
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, Bytes};

use crate::{
    hasher::Hasher,
    key::{Key, KeyGenerator},
};

use super::{topology::Topology, Pos};

#[serde_as]
#[derive(Deserialize, Serialize)]
pub struct Node<H, const N: usize> {
    pub pos: Pos,
    #[serde_as(as = "Bytes")]
    pub key: Key<N>,
    pd: PhantomData<H>,
}

impl<H, const N: usize> HeapSize for Node<H, N> {
    fn heap_size(&self) -> usize {
        self.pos.heap_size() + self.key.heap_size()
    }
}

impl<H, const N: usize> Node<H, N> {
    pub fn new(key: Key<N>) -> Self {
        Self {
            pos: (0, 0),
            key,
            pd: PhantomData,
        }
    }

    pub fn with_rng(mut rng: impl KeyGenerator<N>) -> Self {
        Self::new(rng.gen_key())
    }

    pub fn with_pos(pos: Pos, key: Key<N>) -> Self {
        Self {
            pos,
            key,
            pd: PhantomData,
        }
    }

    pub fn derive(&self, topology: &Topology, pos: Pos) -> Key<N>
    where
        H: Hasher<N>,
    {
        if self.pos == pos {
            self.key
        } else {
            topology
                .derivation_path::<H, N>(self.key, self.pos, pos)
                // .inspect(|(pos, key)| eprintln!("pos = {pos:?}, key = {}", hex::encode(&key)))
                .last()
                .map(|(_, key)| key)
                .unwrap()
        }
    }

    pub fn derive_and_cache(
        &self,
        topology: &Topology,
        pos: Pos,
        read_cache: &mut LruCache<Pos, Key<N>>,
    ) -> Key<N>
    where
        H: Hasher<N>,
    {
        // We're already there.
        if self.pos == pos {
            return self.key;
        }

        // We want to start our derivation from the position of the closest
        // ancestor whose key we've already cached. It's possible that we
        // haven't yet cached anything, which means the position we should start
        // from is simply the current root's position.
        let (start_pos, start_key) = topology
            .path(self.pos, pos)
            .rev()
            .skip_while(|pos| !read_cache.contains(pos))
            .next()
            .map(|pos| (pos, *read_cache.get(&pos).unwrap()))
            .unwrap_or((self.pos, self.key));

        // We've already cached the key.
        if start_pos == pos {
            return start_key;
        }

        // Walk the path from the start position and compute the derived key
        // with a fold starting with the start key. We skip the initial position
        // in the path because the path starts off at the start position.
        topology
            .derivation_path::<H, N>(start_key, start_pos, pos)
            .map(|(pos, key)| {
                read_cache.insert(pos, key).unwrap();
                key
            })
            .last()
            .unwrap()
    }

    pub fn derive_mut_and_cache(
        &self,
        topology: &Topology,
        pos: Pos,
        write_cache: &mut LruCache<Pos, Key<N>>,
        read_cache: &mut LruCache<Pos, Key<N>>,
    ) -> Key<N>
    where
        H: Hasher<N>,
    {
        // We're already there.
        if self.pos == pos {
            return self.key;
        }

        // We want to start our derivation from the position of the closest
        // ancestor whose key we've already cached. It's possible that we
        // haven't yet cached anything, which means the position we should start
        // from is simply the current root's position.
        let (start_pos, start_key) = topology
            .path(self.pos, pos)
            .rev()
            .skip_while(|pos| !write_cache.contains(pos))
            .next()
            .map(|pos| (pos, *write_cache.get(&pos).unwrap()))
            .unwrap_or((self.pos, self.key));

        // We've already cached the key.
        if start_pos == pos {
            return start_key;
        }

        // Walk the path from the start position and compute the derived key
        // with a fold starting with the start key. We skip the initial position
        // in the path because the path starts off at the start position.
        topology
            .derivation_path::<H, N>(start_key, start_pos, pos)
            .map(|(pos, key)| {
                write_cache.insert(pos, key).unwrap();
                read_cache.insert(pos, key).unwrap();
                key
            })
            .last()
            .unwrap()
    }

    pub fn coverage(&self, topology: &Topology, level: u64, start: u64, end: u64) -> Vec<Self>
    where
        H: Hasher<N>,
    {
        topology
            .coverage(level, start, end)
            .map(|pos| Self {
                pos,
                key: self.derive(topology, pos),
                pd: PhantomData,
            })
            .collect()
    }

    pub fn coverage_and_cache(
        &self,
        topology: &Topology,
        level: u64,
        start: u64,
        end: u64,
        cache: &mut LruCache<Pos, Key<N>>,
    ) -> Vec<Self>
    where
        H: Hasher<N>,
    {
        topology
            .coverage(level, start, end)
            .map(|pos| Self {
                pos,
                key: self.derive_and_cache(topology, pos, cache),
                pd: PhantomData,
            })
            .collect()
    }

    pub fn fmt(&self, f: &mut fmt::Formatter<'_>, topology: &Topology) -> fmt::Result
    where
        H: Hasher<N>,
    {
        self.fmt_helper(f, topology, String::new(), self.pos, true)
    }

    fn fmt_helper(
        &self,
        f: &mut fmt::Formatter,
        topology: &Topology,
        prefix: String,
        pos: Pos,
        last: bool,
    ) -> fmt::Result
    where
        H: Hasher<N>,
    {
        if let Some(width) = f.width() {
            write!(f, "{}", " ".repeat(width))?;
        }

        if pos == self.pos {
            write!(f, "> {} ({}, {})", hex::encode(&self.key), pos.0, pos.1)?;
        } else {
            write!(f, "{}{} ", prefix, if last { "└───" } else { "├───" })?;
            write!(
                f,
                "{} ({}, {})",
                hex::encode(self.derive(topology, pos)),
                pos.0,
                pos.1
            )?;
        }

        if self.pos != (0, 0) && pos != (topology.height() - 1, topology.end(self.pos) - 1) {
            writeln!(f)?;
        }

        if pos.0 < topology.height() - 1 {
            for i in 0..topology.fanout(pos.0) {
                let prefix = prefix.clone()
                    + if pos == self.pos {
                        ""
                    } else if last {
                        "     "
                    } else {
                        "│    "
                    };
                self.fmt_helper(
                    f,
                    topology,
                    prefix,
                    (pos.0 + 1, pos.1 * topology.fanout(pos.0) + i),
                    i + 1 == topology.fanout(pos.0),
                )?;
            }
        }

        Ok(())
    }
}

impl<H, const N: usize> fmt::Debug for Node<H, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("pos", &self.pos)
            .field("key", &hex::encode(&self.key))
            .finish()
    }
}

impl<H, const N: usize> Copy for Node<H, N> {}

impl<H, const N: usize> Clone for Node<H, N> {
    fn clone(&self) -> Self {
        Self {
            pos: self.pos,
            key: self.key,
            pd: PhantomData,
        }
    }
}

impl<H, const N: usize> PartialEq for Node<H, N> {
    fn eq(&self, other: &Self) -> bool {
        self.pos == other.pos && self.key == other.key
    }
}
