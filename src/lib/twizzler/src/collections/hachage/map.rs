use crate::alloc::Allocator;
use crate::collections::hachage::raw::{
    RawTable, RawTableAlloc
};
use crate::collections::hachage::{DefaultHashBuilder};
use crate::marker::Invariant;

pub struct PersistentHashMap<K: Invariant, V: Invariant, S = DefaultHashBuilder, A: Allocator = RawTableAlloc> {
    pub(crate) hash_builder: S,
    pub(crate) table: RawTable<(K, V), A>,
}

/*impl<K, V> HashMap<K, V, DefaultHashBuilder> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<K, V, S, A> Default for HashMap<K, V, S, A>
where
    S: Default,
    A: Default + Allocator,
{
    #[cfg_attr(feature = "inline-more", inline)]
    fn default() -> Self {
        Self::with_hasher_in(Default::default(), Default::default())
    }
}

impl<K, V, S, A: Allocator> HashMap<K, V, S, A> {
    #[inline]
    pub fn allocator(&self) -> &A {
        self.table.allocator()
    }

    #[cfg_attr(feature = "inline-more", inline)]
    #[cfg_attr(feature = "rustc-dep-of-std", rustc_const_stable_indirect)]
    pub const fn with_hasher_in(hash_builder: S, alloc: A) -> Self {
        Self {
            hash_builder,
            table: RawTable::new_in(alloc),
        }
    }
}*/