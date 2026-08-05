pub type DefaultHashBuilder = ahash::RandomState;

mod benches;
mod control;
pub mod map;
pub mod raw;

pub use map::{PHMsession, PersistentHashMap};
use raw::HashTableAlloc;
pub type PersistentHashMapBase<K, V, S = DefaultHashBuilder, A = HashTableAlloc> =
    raw::RawTable<(K, V), S, A>;
