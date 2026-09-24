//! elephance-core: the dataset and workload definition shared by every arm of the
//! hydration-crossover and N-reader-sharing experiments.
//!
//! Everything is a pure function of a seed, so no arm needs an oracle file: a query
//! result is verified by recomputing `props_for(key)` and comparing exactly. The three
//! on-disk representations (Twizzler PersistentHashMap objects, flat file, LMDB) are
//! built from the same `(key_at, props_for)` stream and are therefore logically
//! identical by construction.
//!
//! Zero dependencies on purpose: this crate compiles unchanged for the Twizzler target
//! and the Linux baseline host.

/// SplitMix64 increment.
pub const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64 finalizer. Bijective on u64.
#[inline]
pub fn mix(mut z: u64) -> u64 {
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The i-th output of the SplitMix64 stream seeded with `seed`.
#[inline]
pub fn splitmix_at(seed: u64, i: u64) -> u64 {
    mix(seed.wrapping_add(i.wrapping_add(1).wrapping_mul(GOLDEN)))
}

/// 128-bit dataset key. `lo` alone is unique per index (bijective finalizer over
/// distinct states), so the full key is unique; `hi` adds entropy for hashing.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub hi: u64,
    pub lo: u64,
}

impl Key {
    #[inline]
    pub fn as_u128(&self) -> u128 {
        ((self.hi as u128) << 64) | self.lo as u128
    }

    #[inline]
    pub fn from_u128(v: u128) -> Self {
        Key { hi: (v >> 64) as u64, lo: v as u64 }
    }

    #[inline]
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.hi.to_le_bytes());
        b[8..].copy_from_slice(&self.lo.to_le_bytes());
        b
    }

    #[inline]
    pub fn from_bytes(b: &[u8; 16]) -> Self {
        Key {
            hi: u64::from_le_bytes(b[..8].try_into().unwrap()),
            lo: u64::from_le_bytes(b[8..].try_into().unwrap()),
        }
    }
}

/// The i-th key of the dataset seeded with `seed`. Distinct for all i < 2^64.
#[inline]
pub fn key_at(seed: u64, i: u64) -> Key {
    let lo = splitmix_at(seed, i);
    let hi = mix(lo ^ 0xA5A5_A5A5_5A5A_5A5A);
    Key { hi, lo }
}

/// Synthetic material properties, 64 bytes. Field ranges are physically plausible but
/// the values are noise; what matters is that they are an exact pure function of the key.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MatProps {
    /// g/cm^3, [0.5, 25)
    pub density: f64,
    /// eV, [0, 6)
    pub band_gap: f64,
    /// eV/atom, [-4, 0)
    pub formation_energy: f64,
    /// GPa, [1, 400)
    pub bulk_modulus: f64,
    /// GPa, [1, 200)
    pub shear_modulus: f64,
    /// K, [250, 4000)
    pub melting_point: f64,
    /// uV/K, [-200, 200)
    pub seebeck: f64,
    /// Bohr magnetons, [0, 6)
    pub magnetic_moment: f64,
}

pub const PROPS_BYTES: usize = 64;
pub const KEY_BYTES: usize = 16;
pub const RECORD_BYTES: usize = KEY_BYTES + PROPS_BYTES;

/// Map a u64 draw to [0, 1) with 53 bits of precision. Exact and NaN-free.
#[inline]
fn unit(u: u64) -> f64 {
    (u >> 11) as f64 * (1.0 / 9007199254740992.0)
}

/// The properties for a key. Pure; verification recomputes this and compares with `==`
/// (bit-exact: both sides derive the identical f64s, no parsing or rounding anywhere).
pub fn props_for(key: Key) -> MatProps {
    let s = key.lo ^ key.hi.rotate_left(32);
    let d = |j: u64| unit(splitmix_at(s, j));
    MatProps {
        density: 0.5 + 24.5 * d(0),
        band_gap: 6.0 * d(1),
        formation_energy: -4.0 + 4.0 * d(2),
        bulk_modulus: 1.0 + 399.0 * d(3),
        shear_modulus: 1.0 + 199.0 * d(4),
        melting_point: 250.0 + 3750.0 * d(5),
        seebeck: -200.0 + 400.0 * d(6),
        magnetic_moment: 6.0 * d(7),
    }
}

impl MatProps {
    pub fn to_bytes(&self) -> [u8; PROPS_BYTES] {
        let mut b = [0u8; PROPS_BYTES];
        let fields = [
            self.density,
            self.band_gap,
            self.formation_energy,
            self.bulk_modulus,
            self.shear_modulus,
            self.melting_point,
            self.seebeck,
            self.magnetic_moment,
        ];
        for (i, f) in fields.iter().enumerate() {
            b[i * 8..(i + 1) * 8].copy_from_slice(&f.to_le_bytes());
        }
        b
    }

    pub fn from_bytes(b: &[u8; PROPS_BYTES]) -> Self {
        let f = |i: usize| f64::from_le_bytes(b[i * 8..(i + 1) * 8].try_into().unwrap());
        MatProps {
            density: f(0),
            band_gap: f(1),
            formation_energy: f(2),
            bulk_modulus: f(3),
            shear_modulus: f(4),
            melting_point: f(5),
            seebeck: f(6),
            magnetic_moment: f(7),
        }
    }
}

pub fn record_to_bytes(key: Key, props: &MatProps) -> [u8; RECORD_BYTES] {
    let mut b = [0u8; RECORD_BYTES];
    b[..KEY_BYTES].copy_from_slice(&key.to_bytes());
    b[KEY_BYTES..].copy_from_slice(&props.to_bytes());
    b
}

pub fn record_from_bytes(b: &[u8; RECORD_BYTES]) -> (Key, MatProps) {
    let kb: [u8; KEY_BYTES] = b[..KEY_BYTES].try_into().unwrap();
    let pb: [u8; PROPS_BYTES] = b[KEY_BYTES..].try_into().unwrap();
    (Key::from_bytes(&kb), MatProps::from_bytes(&pb))
}

/// Flat-file header (the "hydrate" arm's format): 32 bytes, then `entries` records.
pub const FLAT_MAGIC: [u8; 8] = *b"ELPH0001";
pub const FLAT_HEADER_BYTES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlatHeader {
    pub entries: u64,
    pub seed: u64,
}

impl FlatHeader {
    pub fn to_bytes(&self) -> [u8; FLAT_HEADER_BYTES] {
        let mut b = [0u8; FLAT_HEADER_BYTES];
        b[..8].copy_from_slice(&FLAT_MAGIC);
        b[8..16].copy_from_slice(&self.entries.to_le_bytes());
        b[16..24].copy_from_slice(&self.seed.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; FLAT_HEADER_BYTES]) -> Result<Self, &'static str> {
        if b[..8] != FLAT_MAGIC {
            return Err("bad magic");
        }
        Ok(FlatHeader {
            entries: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            seed: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        })
    }
}

/// The dataset index targeted by the j-th query. Uniform random reads; modulo bias is
/// negligible for any realistic n. Keyed off its own seed so the read pattern is
/// independent of the data.
#[inline]
pub fn query_index(qseed: u64, j: u64, n_entries: u64) -> u64 {
    splitmix_at(qseed ^ 0x51F1_5EED_0DDB_A11 , j) % n_entries
}

/// Which shard a key lives in. `lo` is uniform, so this balances.
#[inline]
pub fn shard_of(key: Key, nshards: u32) -> u32 {
    (key.lo % nshards as u64) as u32
}

/// Default per-shard entry cap for the Twizzler arm. 4M entries of (16B key + 64B
/// props) plus SwissTable power-of-two growth stays well under the 1 GiB object bound.
pub const DEFAULT_PER_SHARD: u64 = 4_000_000;

pub fn shards_for_entries(entries: u64, per_shard_max: u64) -> u32 {
    entries.div_ceil(per_shard_max).max(1) as u32
}

pub const DEFAULT_SEED: u64 = 0xE1E9_0000_0000_0001;
pub const DEFAULT_QSEED: u64 = 0xE1E9_0000_0000_0002;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_unique_and_deterministic() {
        let mut seen = HashSet::new();
        for i in 0..100_000u64 {
            let k = key_at(DEFAULT_SEED, i);
            assert_eq!(k, key_at(DEFAULT_SEED, i));
            assert!(seen.insert(k.as_u128()));
        }
    }

    #[test]
    fn props_ranges_and_determinism() {
        for i in 0..10_000u64 {
            let k = key_at(DEFAULT_SEED, i);
            let p = props_for(k);
            assert_eq!(p, props_for(k));
            assert!(p.density >= 0.5 && p.density < 25.0);
            assert!(p.band_gap >= 0.0 && p.band_gap < 6.0);
            assert!(p.formation_energy >= -4.0 && p.formation_energy < 0.0);
            assert!(p.bulk_modulus >= 1.0 && p.bulk_modulus < 400.0);
            assert!(p.shear_modulus >= 1.0 && p.shear_modulus < 200.0);
            assert!(p.melting_point >= 250.0 && p.melting_point < 4000.0);
            assert!(p.seebeck >= -200.0 && p.seebeck < 200.0);
            assert!(p.magnetic_moment >= 0.0 && p.magnetic_moment < 6.0);
        }
    }

    #[test]
    fn codec_roundtrip() {
        let k = key_at(DEFAULT_SEED, 42);
        let p = props_for(k);
        let (k2, p2) = record_from_bytes(&record_to_bytes(k, &p));
        assert_eq!(k, k2);
        assert_eq!(p, p2);
        let h = FlatHeader { entries: 123, seed: DEFAULT_SEED };
        assert_eq!(h, FlatHeader::from_bytes(&h.to_bytes()).unwrap());
        assert_eq!(k, Key::from_u128(k.as_u128()));
    }

    #[test]
    fn shards_balance() {
        let n = 100_000u64;
        let shards = 8u32;
        let mut counts = vec![0u64; shards as usize];
        for i in 0..n {
            counts[shard_of(key_at(DEFAULT_SEED, i), shards) as usize] += 1;
        }
        let expect = n / shards as u64;
        for c in counts {
            assert!(c > expect * 9 / 10 && c < expect * 11 / 10, "imbalanced: {c} vs {expect}");
        }
    }

    #[test]
    fn query_indices_in_range() {
        for j in 0..10_000u64 {
            assert!(query_index(DEFAULT_QSEED, j, 999) < 999);
        }
    }
}
