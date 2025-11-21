pub type DefaultHashBuilder = ahash::RandomState;

pub mod raw;
mod control;
mod benches;
pub mod map;

pub use map::{PersistentHashMap, PHMsession};

use raw::HashTableAlloc;
pub type PersistentHashMapBase<K, V, S = DefaultHashBuilder, A = HashTableAlloc> =
    raw::RawTable<(K, V), S, A>;
