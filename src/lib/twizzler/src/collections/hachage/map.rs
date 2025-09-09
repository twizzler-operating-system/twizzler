use crate::collections::hachage::raw::*;
use crate::collections::hachage::{DefaultHashBuilder};
use crate::{
    ptr::{RefMut, Ref},
    alloc::Allocator,
    marker::Invariant,
    object::{Object, ObjectBuilder, TypedObject},
};
use crate::Result;
use std::hash::{BuildHasher, Hash};
use equivalent::Equivalent;
use std::marker::PhantomData;

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
    use std::hash::Hasher;
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

pub struct PersistentHashMap<K: Invariant, V: Invariant, S = DefaultHashBuilder, A: Allocator = HashTableAlloc> {
    pub(crate) table: Object<RawTable<(K, V), S, A>>,
}

impl<K: Invariant, V: Invariant> PersistentHashMap<K, V, DefaultHashBuilder, HashTableAlloc> {
    pub fn new() -> Result<Self> {
        let builder = ObjectBuilder::default();
        Self::with_builder(builder)
    }

    pub fn new_persist() -> Result<Self> {
        let builder = ObjectBuilder::default().persist();
        Self::with_builder(builder)
    }

    pub fn with_builder(builder: ObjectBuilder<RawTable<(K, V), DefaultHashBuilder, HashTableAlloc>>) -> Result<Self> {
        let phm = Self::with_hasher_in(builder, Default::default(), Default::default())?;

        // There's a circular dependency if an RawTable attempts to allocate
        // before the object so we need to do part of the allocation afterwards 
        // so that an empty table can be made.
        let mut phm_tx = phm.table.as_tx()?;
        let mut base = phm_tx.base_mut();
        
        base.bootstrap(1)?;

        Ok(phm)
    }
}

impl<K: Invariant, V: Invariant, S, A: Allocator> PersistentHashMap<K, V, S, A> {
    pub fn object(&self) -> &Object<RawTable<(K, V), S, A>> {
        &self.table
    }

    pub fn into_object(self) -> Object<RawTable<(K, V), S, A>> {
        self.table
    }

    pub fn capacity(&self) -> usize {
        self.table.base().capacity()
    }

    #[inline]
    pub fn allocator(&self) -> &A {
        self.table.base().allocator()
    }

    pub fn with_hasher_in(builder: ObjectBuilder<RawTable<(K, V), S, A>>, hasher: S, alloc: A) -> Result<Self> {
        let phm = Self {
            table: builder.build_inplace(|tx| {
                tx.write(RawTable::with_hasher_in(hasher, alloc))
            })?,
        };

        Ok(phm)
    }

    pub fn from(value: Object<RawTable<(K, V), S, A>>) -> Self {
        Self {
            table: value
        }
    }

    pub fn len(&self) -> usize {
        self.table.base().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    
    pub fn ctx(&self) -> CarryCtx {
        self.table.base().carry_ctx()
    }

    pub fn clear(&mut self) -> Result<()> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut().owned();
        
        base.clear();

        Ok(())
    }

    pub fn iter(&self) -> Iter<'_, K, V> {
        unsafe {
            Iter {
                _backing: self.table.base().backing(),
                inner: self.table.base().iter(),
                marker: PhantomData,
            }
        }
    }

    pub fn keys(&self) -> Keys<'_, K, V> {
        Keys { inner: self.iter() }
    }

    pub fn values(&self) -> Values<'_, K, V> {
        Values { inner: self.iter() }
    }

    pub fn iter_mut(&mut self) -> Result<IterMut<'_, K, V>> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut();

        unsafe {
            Ok(IterMut {
                _backing: base.backing_mut().owned(),
                inner: self.table.base().iter(),
                marker: PhantomData,
            })
        }
    }

    pub fn values_mut(&mut self) -> Result<ValuesMut<'_, K, V>> {
        Ok(ValuesMut {
            inner: self.iter_mut()?,
        })
    }
}

impl<K: Invariant + Eq + Hash, V: Invariant, S: BuildHasher, A: Allocator> PersistentHashMap<K, V, S, A> {
    pub fn get<Q>(&self, k: &Q) -> Option<&V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        let ctx = self.ctx();

        match self.get_inner(k, &ctx) {
            Some((_, v)) => {
                Some(v)
            }
            None => None,
        }
    }

    pub fn get_pair<Q>(&self, k: &Q, ctx: &impl Ctx) -> Option<&(K, V)> 
    where
    Q: Hash + Equivalent<K> + ?Sized,
    {
        self.get_inner(k, ctx)
    }

    fn get_inner<Q>(&self, k: &Q, ctx: &impl Ctx) -> Option<&(K, V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {  
        let hash = make_hash::<Q, S>(self.hasher(), k);
        self.table.base().get(hash, equivalent_key(k), ctx)
    }

    pub fn hasher(&self) -> &S {
        self.table.base().hasher()
    }

    fn remove_entry(&mut self, k: &K) -> Option<(K, V)> 
    {
        let hash = make_hash::<K, S>(self.hasher(), k);
        let mut tx = self.table.as_tx().ok()?;
        let mut base = tx.base_mut().owned();

        let ctx = base.carry_ctx_mut(&base);

        base.remove(hash, equivalent_key(k), &ctx)
    }

    pub fn remove(&mut self, k: &K) -> Option<V> {
        match self.remove_entry(k) {
            Some((_ , v)) => Some(v),
            None => None
        }
    }
}

impl<K: Invariant + Eq + Hash, V: Invariant> PersistentHashMap<K, V> {
    pub fn reserve(&mut self, additional: usize) -> Result<()> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut().owned();

        let ctx = base.carry_ctx_mut(&base);

        base.reserve(additional, make_hasher(self.hasher()), &ctx);
        Ok(())
    }
}

pub struct PHMsession<'a, K: Invariant, V: Invariant, S = DefaultHashBuilder, A: Allocator = HashTableAlloc> {
    tx_base: RefMut<'a, RawTable<(K, V), S, A>>,
    imm_base: Ref<'a, RawTable<(K, V), S, A>>,
    ctx: CarryCtxMut<'a>
}

impl<K: Invariant + Eq + Hash, V: Invariant, S: BuildHasher> PHMsession<'_, K, V, S> {
    pub fn insert(&mut self, k: K, v: V) -> Result<Option<V>> {
        let hash = make_hash::<K, S>(self.imm_base.hasher(), &k);

        match self.tx_base.find_or_find_insert_slot(hash, equivalent_key(&k), make_hasher(self.imm_base.hasher()), &self.ctx) {
            Ok(bucket) => {
                let mut tx_ref = bucket.as_tx()?;
                let mut mut_bucket = tx_ref.as_mut();
                Ok(Some(std::mem::replace(&mut mut_bucket.1, v)))
            },
            Err(slot) => unsafe {
                self.tx_base.insert_in_slot(hash, slot, (k, v), &self.ctx);
                Ok(None)
            },
        }
    }

    // The mutable reference can only last as long as the write session
    pub fn get_inner_mut<Q>(&mut self, k: &Q) -> Option<&mut (K, V)>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        let hash = make_hash::<Q, S>(self.imm_base.hasher(), k);
        
        self.tx_base.get_mut(hash, equivalent_key(k), &self.ctx)
    }

    pub fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        match self.get_inner_mut(k) {
            Some((_, v)) => {
                Some(v)
            }
            None => None,
        }
    }
}

impl<K: Invariant + Eq + Hash, V: Invariant, S: BuildHasher> PersistentHashMap<K, V, S, HashTableAlloc> {
    pub fn write_session(&mut self) -> Result<PHMsession<'_, K, V, S>> {
        let mut tx = self.table.as_tx()?;
        let base = tx.base_mut().owned();
        let imm_base = base.as_ref().owned();
        let ctx = base.carry_ctx_mut(&base);

        Ok(PHMsession { 
            tx_base: base, 
            imm_base: imm_base, 
            ctx: ctx
        })

    }
    
    pub fn insert(&mut self, k: K, v: V) -> Result<Option<V>> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut().owned();
        
        let ctx = base.carry_ctx_mut(&base);
        let hash = make_hash::<K, S>(base.hasher(), &k);

        match base.find_or_find_insert_slot(hash, equivalent_key(&k), make_hasher(self.hasher()), &ctx) {
            Ok(bucket) => {
                let mut tx_ref = bucket.as_tx()?;
                let mut mut_bucket = tx_ref.as_mut();
                Ok(Some(std::mem::replace(&mut mut_bucket.1, v)))
            },
            Err(slot) => unsafe {
                base.insert_in_slot(hash, slot, (k, v), &ctx);
                Ok(None)
            },
        }
    }

    pub unsafe fn resize(&mut self, capacity: usize) -> Result<()> {
        let mut tx = self.table.as_tx()?;
        let mut base = tx.base_mut().owned();

        let mut ctx = base.carry_ctx_mut(&base);

        base.resize(capacity, make_hasher(self.hasher()), &mut ctx)?;

        Ok(())
    }
}

pub struct Iter<'a, K: Invariant, V: Invariant> {
    _backing: Ref<'a, u8>,
    inner: RawIter<(K, V)>,
    marker: PhantomData<(&'a K, &'a V)>,
}

impl<'a, K: Invariant, V: Invariant> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        // Avoid `Option::map` because it bloats LLVM IR.
        match self.inner.next() {
            Some(x) => unsafe {
                let r = x.as_ref();
                Some((&r.0, &r.1))
            },
            None => None,
        }
    }
}

pub struct IterMut<'a, K: Invariant, V: Invariant> {
    _backing: RefMut<'a, u8>,
    inner: RawIter<(K, V)>,
    // To ensure invariance with respect to V
    marker: PhantomData<(&'a K, &'a mut V)>,
}

impl<'a, K: Invariant, V: Invariant> Iterator for IterMut<'a, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<(&'a K, &'a mut V)> {
        match self.inner.next() {
            Some(mut x) => unsafe {
                let r = x.as_mut();
                Some((&r.0, &mut r.1))
            },
            None => None,
        }
    }
}

pub struct Keys<'a, K: Invariant, V: Invariant> {
    inner: Iter<'a, K, V>,
}

impl<'a, K: Invariant, V: Invariant> Iterator for Keys<'a, K, V> {
    type Item = &'a K;

    fn next(&mut self) -> Option<&'a K> {
        match self.inner.next() {
            Some((k, _)) => Some(k),
            None => None,
        }
    }
}

pub struct Values<'a, K: Invariant, V: Invariant> {
    inner: Iter<'a, K, V>,
}

impl<'a, K: Invariant, V: Invariant> Iterator for Values<'a, K, V> {
    type Item = &'a V;

    fn next(&mut self) -> Option<&'a V> {
        match self.inner.next() {
            Some((_, v)) => Some(v),
            None => None,
        }
    }
}

pub struct ValuesMut<'a, K: Invariant, V: Invariant> {
    inner: IterMut<'a, K, V>,
}

impl<'a, K: Invariant, V: Invariant> Iterator for ValuesMut<'a, K, V> {
    type Item = &'a mut V;

    fn next(&mut self) -> Option<&'a mut V> {
        match self.inner.next() {
            Some((_, v)) => Some(v),
            None => None,
        }
    }
}