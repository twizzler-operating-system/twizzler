use elephance_core::{shard_of, Key, MatProps};
use twizzler::{
    collections::hachage::PersistentHashMap,
    object::{MapFlags, ObjID, Object, ObjectBuilder},
    Invariant,
};

// u128 has no Invariant impl, so keys/values cross into object memory as these cells.
// The derive is a trivial unchecked impl; every field here is u64/f64 (both Invariant).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Invariant)]
pub struct KeyCell {
    pub hi: u64,
    pub lo: u64,
}

impl From<Key> for KeyCell {
    fn from(k: Key) -> Self {
        KeyCell { hi: k.hi, lo: k.lo }
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Invariant)]
pub struct PropsCell {
    pub density: f64,
    pub band_gap: f64,
    pub formation_energy: f64,
    pub bulk_modulus: f64,
    pub shear_modulus: f64,
    pub melting_point: f64,
    pub seebeck: f64,
    pub magnetic_moment: f64,
}

impl From<MatProps> for PropsCell {
    fn from(p: MatProps) -> Self {
        PropsCell {
            density: p.density,
            band_gap: p.band_gap,
            formation_energy: p.formation_energy,
            bulk_modulus: p.bulk_modulus,
            shear_modulus: p.shear_modulus,
            melting_point: p.melting_point,
            seebeck: p.seebeck,
            magnetic_moment: p.magnetic_moment,
        }
    }
}

impl PropsCell {
    pub fn matches(&self, p: &MatProps) -> bool {
        *self == PropsCell::from(*p)
    }
}

type Map = PersistentHashMap<KeyCell, PropsCell>;

/// A dataset sharded across PersistentHashMap objects, each under the 1 GiB object
/// bound, named `{base}-{k}` in the naming service. Shard count is discovered on open
/// by probing names in order.
pub struct Sharded {
    shards: Vec<Map>,
}

fn map_flags(persist: bool) -> MapFlags {
    let f = MapFlags::READ | MapFlags::WRITE;
    if persist {
        f | MapFlags::PERSIST
    } else {
        f
    }
}

impl Sharded {
    pub fn create(base: &str, nshards: u32, persist: bool, reserve_per: usize) -> Self {
        let nh = naming::dynamic_naming_factory().unwrap();
        let mut shards = Vec::with_capacity(nshards as usize);
        for k in 0..nshards {
            let mut phm = Map::with_builder(ObjectBuilder::default().persist(persist))
                .expect("shard create failed");
            phm.reserve(reserve_per).expect("shard reserve failed");
            let name = format!("{base}-{k}");
            let _ = nh.remove(&name);
            nh.put(&name, phm.object().id()).expect("naming put failed");
            shards.push(phm);
        }
        Sharded { shards }
    }

    pub fn open(base: &str, persist: bool) -> Self {
        let nh = naming::dynamic_naming_factory().unwrap();
        let mut shards = Vec::new();
        loop {
            let name = format!("{base}-{}", shards.len());
            let Ok(node) = nh.get(&name, naming::GetFlags::empty()) else {
                break;
            };
            let obj = Object::map(node.id, map_flags(persist)).expect("shard map failed");
            shards.push(Map::from(obj));
        }
        assert!(!shards.is_empty(), "no shards found under {base}");
        Sharded { shards }
    }

    pub fn nshards(&self) -> u32 {
        self.shards.len() as u32
    }

    /// Bucket a batch by shard, then insert each bucket under one write session — one
    /// transaction and therefore one blocking durability round trip per shard per
    /// batch, instead of one per insert (per-insert `PersistentHashMap::insert` opens
    /// a TxObject per call, which syncs on drop: ~1.1 ms/op measured).
    pub fn insert_batch(&mut self, items: impl Iterator<Item = (Key, MatProps)>) {
        let n = self.nshards();
        let mut buckets: Vec<Vec<(KeyCell, PropsCell)>> = (0..n).map(|_| Vec::new()).collect();
        for (key, props) in items {
            buckets[shard_of(key, n) as usize].push((key.into(), props.into()));
        }
        for (k, bucket) in buckets.into_iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let mut sess = self.shards[k]
                .write_session()
                .expect("write session failed");
            for (key, val) in bucket {
                sess.insert(key, val).expect("insert failed");
            }
        }
    }

    pub fn get(&self, key: Key) -> Option<&PropsCell> {
        let k = shard_of(key, self.nshards()) as usize;
        self.shards[k].get(&KeyCell::from(key))
    }

    pub fn lens(&self) -> Vec<usize> {
        self.shards.iter().map(|s| s.len()).collect()
    }

    pub fn ids(&self) -> Vec<ObjID> {
        self.shards.iter().map(|s| s.object().id()).collect()
    }
}
