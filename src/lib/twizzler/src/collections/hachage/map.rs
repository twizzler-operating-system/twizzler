use core::hash;

use crate::collections::hachage::raw::*;
use crate::collections::hachage::{DefaultHashBuilder};
use crate::object::RawObject;
use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{Invariant, StoreCopy},
    object::{Object, ObjectBuilder, TypedObject},
    ptr::{Ref, RefSlice},
};
use crate::Result;
use std::hash::{BuildHasher, Hash};
use equivalent::Equivalent;
use twizzler_abi::syscall::ObjectCreate;

pub(crate) fn make_hasher<Q, V, S>(hash_builder: &S) -> impl Fn(&(Q, V)) -> u64 + '_
where
    Q: Hash,
    S: BuildHasher,
{
    move |val| make_hash::<Q, S>(hash_builder, &val.0)
}

pub(crate) fn make_hash<Q, S>(hash_builder: &S, val: &Q) -> u64
where
    Q: Hash + ?Sized,
    S: BuildHasher,
{
    use core::hash::Hasher;
    let mut state = hash_builder.build_hasher();
    val.hash(&mut state);
    state.finish()
}

pub(crate) fn equivalent_key<Q, K, V>(k: &Q) -> impl Fn(&(K, V)) -> bool + '_
where
    Q: Equivalent<K> + ?Sized,
{
    move |x| k.equivalent(&x.0)
}

pub struct PHMBuilder<K: Invariant, V: Invariant, S = DefaultHashBuilder, A: Allocator = HashTableAlloc> {
    builder: Option<ObjectBuilder<RawTable<(K, V), S, A>>>,
    capacity: usize, 
    allocator: Option<A>,
    hasher: Option<S>
}

pub struct PersistentHashMap<K: Invariant, V: Invariant, S = DefaultHashBuilder, A: Allocator = HashTableAlloc> {
    pub(crate) table: Object<RawTable<(K, V), S, A>>
}

impl<K: Invariant, V: Invariant> PersistentHashMap<K, V, DefaultHashBuilder, HashTableAlloc> {
    pub fn new() -> Result<Self> {
        let builder = ObjectBuilder::default();
        Self::with_builder(builder)
    }

    pub fn with_builder(builder: ObjectBuilder<RawTable<(K, V), DefaultHashBuilder, HashTableAlloc>>) -> Result<Self> {
        Self::with_hasher_in(builder, Default::default(), Default::default())
    }
}

impl<K: Invariant, V: Invariant, S> PersistentHashMap<K, V, S> {
    pub fn with_capacity_and_hasher(builder: ObjectBuilder<RawTable<(K, V), S, HashTableAlloc>>, hasher: S, capacity: usize) -> Result<Self> {
        Self::with_capacity_in(builder, hasher, HashTableAlloc::default(), capacity)
    }
}

impl<K: Invariant, V: Invariant, S, A: Allocator> PersistentHashMap<K, V, S, A> {
    #[inline]
    pub fn allocator(&self) -> &A {
        self.table.base().allocator()
    }

    pub fn with_alloc_in(builder: ObjectBuilder<RawTable<(K, V), S, A>>, alloc: A) -> Result<Self> {
        todo!()
    }

    pub fn with_hasher_in(builder: ObjectBuilder<RawTable<(K, V), S, A>>, hasher: S, alloc: A) -> Result<Self> {
        Ok(Self {
            table: builder.build_inplace(|tx| {
                tx.write(RawTable::with_hasher_in(hasher, alloc))
            })?,
        })
    }

    pub fn with_capacity_in(builder: ObjectBuilder<RawTable<(K, V), S, A>>, hasher: S, alloc: A, capacity: usize) -> Result<Self> {
        todo!()
        /*// Hack because alloc needs to exist in the object before any allocation can happen
        let dummy = Self {
            table: builder.build_inplace(|tx: crate::tx::TxObject<std::mem::MaybeUninit<RawTable<(K, V), S, A>>>| {
                tx.id()
            })?
        };
        
        Ok(Self {
            table: builder.build_inplace(|tx: crate::tx::TxObject<std::mem::MaybeUninit<RawTable<(K, V), S, A>>>| {
                let table = RawTable::with_hasher_in(hasher, dummy.allocator());
                let r = tx.write(table)?;
            })?
        })*/
    }
}

impl<K: Invariant, V: Invariant, S: BuildHasher, A: Allocator> PersistentHashMap<K, V, S, A> {

}

impl<K: Invariant + Eq + Hash, V: Invariant, S: BuildHasher, A: Allocator> PersistentHashMap<K, V, S, A> {
    pub fn hasher(&self) -> &S {
        self.table.base().hasher()
    }

    pub fn insert(&mut self, k: K, v: V) -> Result<Option<V>> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut().owned();
        
        let hash = make_hash::<K, S>(self.hasher(), &k);

        match base.find_or_find_insert_slot(hash, equivalent_key(&k), make_hasher(self.hasher())) {
            Ok(bucket) => {
                let mut tx_ref = bucket.as_tx()?;
                let mut mut_bucket = tx_ref.as_mut();
                Ok(Some(std::mem::replace( unsafe { &mut mut_bucket.1 }, v)))
            },
            Err(slot) => unsafe {
                base.insert_in_slot(hash, slot, (k, v));
                Ok(None)
            },
        }
    }

    pub unsafe fn resize(&mut self, capacity: usize) -> Result<()> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut().owned();

        base.resize(capacity, make_hasher(self.hasher()));

        Ok(())
    }

    /*pub(crate) fn find_or_find_insert_slot<Q: Equivalent<K> + ?Sized>(
        &mut self,
        hash: u64,
        key: &Q,
        tx: impl AsRef<TxObject>
    ) -> std::result::Result<Ref<(K, V)>, usize> {
       todo!()
    }*/

    pub fn get<Q>(&self, k: &Q) -> Option<&V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        match self.get_inner(k) {
            Some((_, v)) => {
                Some(v)
            }
            None => None,
        }
    }

    pub fn get_pair<Q>(&self, k: &Q) -> Option<&(K, V)> 
    where
    Q: Hash + Equivalent<K> + ?Sized,
    {
        self.get_inner(k)
    }

    fn get_inner<Q>(&self, k: &Q) -> Option<&(K, V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {  
        let hash = make_hash::<Q, S>(self.hasher(), k);
        self.table.base().get(hash, equivalent_key(k))
    }
}

