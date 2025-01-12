//! This crate implements an LRU (least-recently-used) cache that is limited by
//! the total size of its entries. As more entries are added than fit in the
//! specified memory bound, the least-recently-used ones are ejected. The cache
//! supports average-case O(1) insertion, retrieval, and removal.
//!
//! Note that the memory required for each entry is only an estimate and some
//! auxiliary structure is disregarded. With some data structures (such as the
//! [HashMap](std::collections::HashMap) or
//! [HashSet](std::collections::HashSet)), some internal data is not
//! accessible, so the required memory is even more underestimated. Therefore,
//! the actual data structure can take more memory than was assigned, however
//! this should not be an excessive amount in most cases.
//!
//! # Motivating example
//!
//! Imagine we are building a web server that sends large responses to clients.
//! To reduce the load, they are split into sections and the client is given a
//! token to access the different sections individually. However, recomputing
//! the sections on each request leads to too much server load, so they need to
//! be cached. An LRU cache is useful in this situation, as clients are most
//! likely to request new sections temporally localized.
//!
//! Now consider the situation when most responses are very small, but some may
//! be large. This would either lead to the cache being conservatively sized
//! and allow for less cached responses than would normally be possible, or to
//! the cache being liberally sized and potentially overflow memory if too many
//! large responses have to be cached. To prevent this, the cache is designed
//! with an upper bound on its memory instead of the number of elements.
//!
//! The code below shows how the basic structure might look like.
//!
//! ```
//! use lru_mem::LruCache;
//!
//! struct WebServer {
//!     cache: LruCache<u128, Vec<String>>
//! }
//!
//! fn random_token() -> u128 {
//!     // A cryptographically secure random token.
//!     42
//! }
//!
//! fn generate_sections(input: String) -> Vec<String> {
//!     // A complicated set of sections that is highly variable in size.
//!     vec![input.clone(), input]
//! }
//!
//! impl WebServer {
//!     fn new(max_size: usize) -> WebServer {
//!         // Create a new web server with a cache that holds at most max_size
//!         // bytes of elements.
//!         WebServer {
//!             cache: LruCache::new(max_size)
//!         }
//!     }
//!
//!     fn on_query(&mut self, input: String) -> u128 {
//!         // Generate sections, store them in the cache, and return token.
//!         let token = random_token();
//!         let sections = generate_sections(input);
//!         self.cache.insert(token, sections)
//!             .expect("sections do not fit in the cache");
//! 
//!         token
//!     }
//!
//!     fn on_section_request(&mut self, token: u128, index: usize)
//!             -> Option<&String> {
//!         // Lookup the token and get the section with given index.
//!         self.cache.get(&token).and_then(|s| s.get(index))
//!     }
//! }
//! ```
//!
//! For further details on how to use the cache, see the [LruCache] struct.

use std::borrow::Borrow;
use std::fmt::{self, Debug, Formatter};
use std::hash::{BuildHasher, Hash};
use std::mem;

use hashbrown::hash_map::DefaultHashBuilder;
use hashbrown::raw::RawTable;
use hashbrown::TryReserveError;

use entry::{Entry, EntryPtr, UnhingedEntry};
pub use entry::entry_size;
pub use error::{InsertError, MutateError, TryInsertError};
pub use iter::{Drain, IntoIter, IntoKeys, IntoValues, Iter, Keys, Values};
pub use mem_size::{HeapSize, MemSize, ValueSize};

mod entry;
mod error;
mod iter;
mod mem_size;

/// An LRU (least-recently-used) cache that stores values associated with keys.
/// Insertion, retrieval, and removal all have average-case complexity in O(1).
/// The cache has an upper memory bound, which is set at construction time.
/// This is enforced using estimates on the memory requirement of each
/// key-value-pair. Note that some auxiliary data structures may allocate more
/// memory. So, this data structure may require more than the given limit.
///
/// Each time a new entry is added with [LruCache::insert], it is checked
/// whether it fits in the given memory bound. If it does not, the
/// least-recently-used element is dropped from the cache, until the new entry
/// fits.
///
/// Note that both the key type `K` and the value type `V` must implement the
/// [MemSize] trait to allow for size estimation in normal usage. In addition,
/// the key type `K` is required to implement [Hash] and [Eq] for most
/// meaningful operations.
///
/// Furthermore, the hasher type `S` must implement the [BuildHasher] trait for
/// non-trivial functionality.
///
/// Mutable access is not allowed directly, since it may change the size of an
/// entry. It must be done either by removing the element using
/// [LruCache::remove] and inserting it again, or passing a mutating closure to
/// [LruCache::mutate].
pub struct LruCache<K, V, S = DefaultHashBuilder> {
    table: RawTable<Entry<K, V>>,

    // The seal is a dummy entry that is simultaneously in front of the head
    // and behind the tail of the list. You can imagine it as connecting the
    // list to a cycle.
    // Having a dummy entry makes some operations (in particular unhinging)
    // simpler. The cache would also work with two dummy entries, but only the
    // next-field of the head and prev-field of the tail would be used. So, we
    // use one dummy entry to act as both. It has to be ensured that we never
    // iterate over this seal, so the edge case in which the cache is empty has
    // to be considered in every situation where we iterate over the elements.

    // This system is inspired by the lru-crate: https://crates.io/crates/lru

    seal: EntryPtr<K, V>,
    current_size: usize,
    max_size: usize,
    hash_builder: S
}

impl<K, V> LruCache<K, V> {

    /// Creates a new, empty LRU cache with the given maximum memory size.
    ///
    /// # Arguments
    ///
    /// * `max_size`: The maximum number of bytes that the sum of the memory
    /// estimates of all entries may occupy. It is important to note that this
    /// bound may be exceeded in total memory requirement of the created data
    /// structure.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// // Create an LRU cache with 16 KiB memory limit
    /// let cache: LruCache<String, String> = LruCache::new(16 * 1024);
    /// ```
    pub fn new(max_size: usize) -> LruCache<K, V> {
        LruCache::with_table_and_hasher(max_size, RawTable::new(),
            DefaultHashBuilder::default())
    }

    /// Creates a new, empty LRU cache with the given maximum memory size and
    /// the specified initial capacity.
    ///
    /// # Arguments
    ///
    /// * `max_size`: The maximum number of bytes that the sum of the memory
    /// estimates of all entries may occupy. It is important to note that this
    /// bound may be exceeded in total memory requirement of the created data
    /// structure.
    /// * `capacity`: A lower bound on the number of elements that the cache
    /// will be able to hold without reallocating.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// // Create an LRU with 4 KiB memory limit that can hold at least 8
    /// // elements without reallocating.
    /// let cache: LruCache<String, String> = LruCache::with_capacity(4096, 8);
    /// ```
    pub fn with_capacity(max_size: usize, capacity: usize) -> LruCache<K, V> {
        LruCache::with_table_and_hasher(max_size,
            RawTable::with_capacity(capacity), DefaultHashBuilder::default())
    }
}

impl<K, V, S> LruCache<K, V, S> {

    fn with_table_and_hasher(max_size: usize, table: RawTable<Entry<K, V>>,
            hash_builder: S) -> LruCache<K, V, S> {
        let seal = EntryPtr::new_seal();

        LruCache {
            table,
            seal,
            current_size: 0,
            max_size,
            hash_builder
        }
    }

    /// Creates a new, empty LRU cache with the given maximum memory size which
    /// will use the given hash builder to hash keys.
    ///
    /// # Arguments
    ///
    /// * `max_size`: The maximum number of bytes that the sum of the memory
    /// estimates of all entries may occupy. It is important to note that this
    /// bound may be exceeded in total memory requirement of the created data
    /// structure.
    /// * `hash_builder`: The hasher used to hash keys. It should implement the
    /// [BuildHasher] trait to allow operations being applied to the cache.
    ///
    /// # Example
    ///
    /// ```
    /// use hashbrown::hash_map::DefaultHashBuilder;
    /// use lru_mem::LruCache;
    ///
    /// // Create an LRU with 4 KiB memory limit that uses s for hashing keys.
    /// let s = DefaultHashBuilder::default();
    /// let cache: LruCache<String, String> = LruCache::with_hasher(4096, s);
    /// ```
    pub fn with_hasher(max_size: usize, hash_builder: S) -> LruCache<K, V, S> {
        LruCache::with_table_and_hasher(max_size, RawTable::new(),
            hash_builder)
    }

    /// Creates a new, empty LRU cache with the given maximum memory size and
    /// the specified initial capacity which will use the given hash builder to
    /// hash keys.
    ///
    /// # Arguments
    ///
    /// * `max_size`: The maximum number of bytes that the sum of the memory
    /// estimates of all entries may occupy. It is important to note that this
    /// bound may be exceeded in total memory requirement of the created data
    /// structure.
    /// * `capacity`: A lower bound on the number of elements that the cache
    /// will be able to hold without reallocating.
    /// * `hash_builder`: The hasher used to hash keys. It should implement the
    /// [BuildHasher] trait to allow operations being applied to the cache.
    ///
    /// # Example
    ///
    /// ```
    /// use hashbrown::hash_map::DefaultHashBuilder;
    /// use lru_mem::LruCache;
    ///
    /// // Create an LRU with 4 KiB memory limit that can hold at least 8
    /// // elements without reallocating that uses s for hashing keys.
    /// let s = DefaultHashBuilder::default();
    /// let cache: LruCache<String, String> =
    ///     LruCache::with_capacity_and_hasher(4096, 8, s);
    /// ```
    pub fn with_capacity_and_hasher(max_size: usize, capacity: usize,
            hash_builder: S) -> LruCache<K, V, S> {
        LruCache::with_table_and_hasher(max_size,
            RawTable::with_capacity(capacity), hash_builder)
    }

    /// Gets the maximum number of bytes that the sum of the memory estimates
    /// of all entries may occupy. It is important to note that this bound may
    /// be exceeded in total memory requirement of the created data structure.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let cache: LruCache<String, String> = LruCache::new(65536);
    /// assert_eq!(65536, cache.max_size());
    /// ```
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Gets the current estimated memory of all entries contained in this
    /// cache, in bytes.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> = LruCache::new(1024);
    /// assert_eq!(0, cache.current_size());
    ///
    /// // The exact amount of occupied memory depends not only on the values,
    /// // but also some auxiliary data of the internal structures.
    /// cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
    /// assert!(cache.current_size() > 0);
    /// ```
    pub fn current_size(&self) -> usize {
        self.current_size
    }

    /// Gets the number of entries contained in this cache.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> = LruCache::new(1024);
    /// assert_eq!(0, cache.len());
    ///
    /// cache.insert("apple".to_owned(), "banana".to_owned()).unwrap();
    /// assert_eq!(1, cache.len());
    /// ```
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Indicates whether this cache is empty, i.e. its length
    /// ([LruCache::len]) is zero.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> = LruCache::new(1024);
    /// assert!(cache.is_empty());
    ///
    /// cache.insert("apple".to_owned(), "banana".to_owned()).unwrap();
    /// assert!(!cache.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of elements the cache can hold without reallocating.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> =
    ///     LruCache::with_capacity(1024, 10);
    /// assert!(cache.capacity() >= 10);
    /// ```
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }

    /// Returns a reference to the cache's [BuildHasher].
    ///
    /// # Example
    ///
    /// ```
    /// use hashbrown::hash_map::DefaultHashBuilder;
    /// use lru_mem::LruCache;
    ///
    /// let hasher = DefaultHashBuilder::default();
    /// let cache: LruCache<String, String> =
    ///     LruCache::with_hasher(4096, hasher);
    /// let hasher: &DefaultHashBuilder = cache.hasher();
    /// ```
    pub fn hasher(&self) -> &S {
        &self.hash_builder
    }

    /// Removes all elements from this cache.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.clear();
    ///
    /// assert_eq!(None, cache.get("lemon"));
    /// assert_eq!(0, cache.len());
    /// assert_eq!(0, cache.current_size());
    /// ```
    pub fn clear(&mut self) {
        for entry in self.table.drain() {
            unsafe { entry.drop(); }
        }

        self.current_size = 0;
        self.seal.get_mut().next = self.seal;
        self.seal.get_mut().prev = self.seal;
    }

    /// Creates an iterator over the entries (keys and values) contained in
    /// this cache, ordered from least- to most-recently-used. The values are
    /// not touched, i.e. the usage history is not altered in any way. That is,
    /// the semantics are as in [LruCache::peek].
    ///
    /// The memory requirement for any key or value may not be changed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("grapefruit".to_owned(), "bitter".to_owned()).unwrap();
    /// let mut iter = cache.iter();
    ///
    /// assert_eq!(Some((&"apple".to_owned(), &"sweet".to_owned())),
    ///     iter.next());
    /// assert_eq!(Some((&"grapefruit".to_owned(), &"bitter".to_owned())),
    ///     iter.next_back());
    /// assert_eq!(Some((&"lemon".to_owned(), &"sour".to_owned())),
    ///     iter.next());
    /// assert_eq!(None, iter.next());
    /// assert_eq!(None, iter.next_back());
    /// ```
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter::new(self)
    }

    /// Creates an iterator over the keys contained in this cache, ordered from
    /// least- to most-recently-used. The values are not touched, i.e. the
    /// usage history is not altered in any way. That is, the semantics are as
    /// in [LruCache::peek].
    ///
    /// The memory requirement for any key may not be changed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("grapefruit".to_owned(), "bitter".to_owned()).unwrap();
    /// let mut keys = cache.keys();
    ///
    /// assert_eq!(Some(&"apple".to_owned()), keys.next());
    /// assert_eq!(Some(&"grapefruit".to_owned()), keys.next_back());
    /// assert_eq!(Some(&"lemon".to_owned()), keys.next());
    /// assert_eq!(None, keys.next());
    /// assert_eq!(None, keys.next_back());
    /// ```
    pub fn keys(&self) -> Keys<'_, K, V> {
        Keys::new(self)
    }

    /// Creates an iterator over the values contained in this cache, ordered
    /// from least- to most-recently-used. The values are not touched, i.e. the
    /// usage history is not altered in any way. That is, the semantics are as
    /// in [LruCache::peek].
    ///
    /// The memory requirement for any value may not be changed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("grapefruit".to_owned(), "bitter".to_owned()).unwrap();
    /// let mut values = cache.values();
    ///
    /// assert_eq!(Some(&"sweet".to_owned()), values.next());
    /// assert_eq!(Some(&"bitter".to_owned()), values.next_back());
    /// assert_eq!(Some(&"sour".to_owned()), values.next());
    /// assert_eq!(None, values.next());
    /// assert_eq!(None, values.next_back());
    /// ```
    pub fn values(&self) -> Values<'_, K, V> {
        Values::new(self)
    }

    /// Creates an iterator that drains entries from this cache. Both key and
    /// value of each entry are returned. The cache is cleared afterward.
    ///
    /// Note it is important for the drain to be dropped in order to ensure
    /// integrity of the data structure. Preventing it from being dropped, e.g.
    /// using [mem::forget](mem::forget), can result in unexpected behavior of
    /// the cache.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("grapefruit".to_owned(), "bitter".to_owned()).unwrap();
    /// let mut vec = cache.drain().collect::<Vec<_>>();
    ///
    /// assert_eq!(&("apple".to_owned(), "sweet".to_owned()), &vec[0]);
    /// assert_eq!(&("lemon".to_owned(), "sour".to_owned()), &vec[1]);
    /// assert_eq!(&("grapefruit".to_owned(), "bitter".to_owned()), &vec[2]);
    /// assert!(cache.is_empty());
    /// ```
    pub fn drain(&mut self) -> Drain<'_, K, V, S> {
        Drain::new(self)
    }

    /// Creates an iterator that takes ownership of the cache and iterates over
    /// its keys, ordered from least- to most-recently-used.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("grapefruit".to_owned(), "bitter".to_owned()).unwrap();
    /// let mut keys = cache.into_keys().collect::<Vec<_>>();
    ///
    /// assert_eq!(&"apple".to_owned(), &keys[0]);
    /// assert_eq!(&"lemon".to_owned(), &keys[1]);
    /// assert_eq!(&"grapefruit".to_owned(), &keys[2]);
    /// ```
    pub fn into_keys(self) -> IntoKeys<K, V, S> {
        IntoKeys::new(self)
    }

    /// Creates an iterator that takes ownership of the cache and iterates over
    /// its values, ordered from least- to most-recently-used.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("grapefruit".to_owned(), "bitter".to_owned()).unwrap();
    /// let mut values = cache.into_values().collect::<Vec<_>>();
    ///
    /// assert_eq!(&"sweet".to_owned(), &values[0]);
    /// assert_eq!(&"sour".to_owned(), &values[1]);
    /// assert_eq!(&"bitter".to_owned(), &values[2]);
    /// ```
    pub fn into_values(self) -> IntoValues<K, V, S> {
        IntoValues::new(self)
    }
}

fn make_hash<K, S>(hash_builder: &S, val: &K) -> u64
where
    K: Hash + ?Sized,
    S: BuildHasher,
{
    use core::hash::Hasher;
    let mut state = hash_builder.build_hasher();
    val.hash(&mut state);
    state.finish()
}

fn make_insert_hash<K, S>(hash_builder: &S, val: &K) -> u64
where
    K: Hash,
    S: BuildHasher,
{
    use core::hash::Hasher;
    let mut state = hash_builder.build_hasher();
    val.hash(&mut state);
    state.finish()
}

fn make_hasher<K, V, S>(hash_builder: &S) -> impl Fn(&Entry<K, V>) -> u64 + '_
where
    K: Hash,
    S: BuildHasher
{
    move |val| make_hash::<K, S>(hash_builder, unsafe { val.key() })
}

fn equivalent_key<Q, K, V>(k: &Q) -> impl Fn(&Entry<K, V>) -> bool + '_
where
    K: Borrow<Q>,
    Q: ?Sized + Eq,
{
    move |x| k.eq(unsafe { x.key() }.borrow())
}

impl<K, V, S> LruCache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher
{
    fn remove_from_table<Q>(&mut self, key: &Q) -> Option<Entry<K, V>>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        let hash = make_hash::<Q, S>(&self.hash_builder, key);
        self.table.remove_entry(hash, equivalent_key(key))
    }

    fn get_from_table<Q>(&self, key: &Q) -> Option<&Entry<K, V>>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        let hash = make_hash::<Q, S>(&self.hash_builder, key);
        self.table.get(hash, equivalent_key(key))
    }

    fn get_mut_from_table<Q>(&mut self, key: &Q) -> Option<&mut Entry<K, V>>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        let hash = make_hash::<Q, S>(&self.hash_builder, key);
        self.table.get_mut(hash, equivalent_key(key))
    }

    #[inline]
    fn insert_into_table_with_hash(&mut self, hash: u64, entry: Entry<K, V>)
            -> Result<EntryPtr<K, V>, Entry<K, V>> {
        match self.table.try_insert_no_grow(hash, entry) {
            Ok(bucket) => Ok(EntryPtr::new(bucket.as_ptr())),
            Err(entry) => Err(entry)
        }
    }

    /// Assumes that there is no entry with the same key in the table. If
    /// insertion works, returns a pointer to the entry inside the table.
    /// Otherwise, returns the entry input into this function.
    fn insert_into_table(&mut self, entry: Entry<K, V>)
            -> Result<EntryPtr<K, V>, Entry<K, V>> {
        let key = unsafe { entry.key() };
        let hash = make_insert_hash::<K, S>(&self.hash_builder, key);

        self.insert_into_table_with_hash(hash, entry)
    }

    fn set_head(&mut self, mut entry: EntryPtr<K, V>) {
        entry.insert(self.seal, self.seal.get().next);
    }

    fn touch_ptr(&mut self, entry: EntryPtr<K, V>) {
        unsafe { entry.unhinge(); }
        self.set_head(entry);
    }

    /// Safety: Requires the key and value of the entry to be initialized.
    unsafe fn remove_metadata(&mut self, entry: Entry<K, V>) -> (K, V) {
        let entry = entry.unhinge();
        self.current_size -= entry.size();

        entry.into_key_value()
    }

    /// Safety: Requires the key of the entry pointed to by the pointer to be
    /// initialized, and the key and value of the entry located at that key in
    /// the hash table to be initialized.
    #[inline]
    unsafe fn remove_ptr(&mut self, entry: EntryPtr<K, V>) -> (K, V) {
        let entry = self.remove_from_table(entry.get().key()).unwrap();
        self.remove_metadata(entry)
    }

    fn eject_to_target(&mut self, target: usize) {
        while self.current_size > target {
            self.remove_lru();
        }
    }

    fn insert_untracked(&mut self, entry: Entry<K, V>) {
        let entry_ptr = unsafe {
            self.insert_into_table(entry).unwrap_unchecked()
        };
        self.set_head(entry_ptr);
    }

    fn try_reallocate(&mut self, new_capacity: usize) -> Result<(), TryReserveError> {
        let hasher = make_hasher(&self.hash_builder);
        let mut old_table = RawTable::try_with_capacity(new_capacity)?;
        mem::swap(&mut self.table, &mut old_table);

        for entry in old_table.into_iter() {
            let mut prev_entry = entry.prev;
            let mut next_entry = entry.next;
            let bucket = self.table.insert(hasher(&entry), entry, &hasher);
            let entry_ptr = EntryPtr::new(bucket.as_ptr());
            prev_entry.get_mut().next = entry_ptr;
            next_entry.get_mut().prev = entry_ptr;
        }

        Ok(())
    }

    fn reallocate(&mut self, new_capacity: usize) {
        self.try_reallocate(new_capacity).unwrap()
    }

    fn lru_ptr(&self) -> Option<EntryPtr<K, V>> {
        let lru = self.seal.get().prev;

        if lru == self.seal {
            None
        }
        else {
            Some(lru)
        }
    }

    fn mru_ptr(&self) -> Option<EntryPtr<K, V>> {
        let mru = self.seal.get().next;

        if mru == self.seal {
            None
        }
        else {
            Some(mru)
        }
    }

    /// Removes the least-recently-used value from this cache. This returns
    /// both key and value of the removed value. If this cache is empty, `None`
    /// is returned.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some(("apple".to_owned(), "sweet".to_owned())),
    ///     cache.remove_lru());
    /// assert_eq!(1, cache.len());
    /// ```
    pub fn remove_lru(&mut self) -> Option<(K, V)> {
        self.lru_ptr().map(|ptr| unsafe { self.remove_ptr(ptr) })
    }

    /// Gets a reference to the least-recently-used entry from this cache. This
    /// returns both key and value of the entry. If the cache is empty, `None`
    /// is returned.
    ///
    /// This method also marks the value as most-recently-used. If you want the
    /// usage history to not be updated, use [LruCache::peek_lru] instead.
    ///
    /// The memory requirement of the key and value may not be changed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some((&"apple".to_owned(), &"sweet".to_owned())),
    ///     cache.get_lru());
    /// assert_eq!(Some((&"lemon".to_owned(), &"sour".to_owned())),
    ///     cache.get_lru());
    /// ```
    pub fn get_lru(&mut self) -> Option<(&K, &V)> {
        self.lru_ptr().map(|entry| {
            unsafe {
                self.touch_ptr(entry);
                let entry = entry.get_extended();
                (entry.key(), entry.value())
            }
        })
    }

    /// Gets a reference to the least-recently-used entry from this cache. This
    /// returns both key and value of the entry. If the cache is empty, `None`
    /// is returned.
    ///
    /// This method does not mark the value as most-recently-used. If you want
    /// the usage history to be updated, use [LruCache::get_lru] instead.
    ///
    /// The memory requirement of the key and value may not be changed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some((&"apple".to_owned(), &"sweet".to_owned())),
    ///     cache.peek_lru());
    /// assert_eq!(Some((&"apple".to_owned(), &"sweet".to_owned())),
    ///     cache.peek_lru());
    /// ```
    pub fn peek_lru(&self) -> Option<(&K, &V)> {
        self.lru_ptr().map(|ptr|
            unsafe {
                let entry = ptr.get_extended();
                (entry.key(), entry.value())
            })
    }

    /// Removes the most-recently-used value from the cache. This returns both
    /// key and value of the removed entry. If the cache is empty, `None` is
    /// returned.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some(("lemon".to_owned(), "sour".to_owned())),
    ///     cache.remove_mru());
    /// assert_eq!(1, cache.len());
    /// ```
    pub fn remove_mru(&mut self) -> Option<(K, V)> {
        self.mru_ptr().map(|ptr| unsafe { self.remove_ptr(ptr) })
    }

    /// Gets a reference to the most-recently-used entry from this cache. This
    /// returns both key and value of the entry. If the cache is empty, `None`
    /// is returned.
    ///
    /// Note that, for the most-recently-used entry, it does not matter whether
    /// the usage history is updated, since it was most-recently-used before.
    /// So, there is no need for a `get_mru` method.
    ///
    /// The memory requirement of the key and value may not be changed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some((&"lemon".to_owned(), &"sour".to_owned())),
    ///     cache.peek_mru());
    /// ```
    pub fn peek_mru(&self) -> Option<(&K, &V)> {
        self.mru_ptr().map(|ptr|
            unsafe {
                let entry = ptr.get_extended();
                (entry.key(), entry.value())
            })
    }

    fn new_capacity(&self, additional: usize)
            -> Result<usize, TryReserveError> {
        self.len().checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)
    }

    /// Reserves capacity for at least `additional` new entries to be inserted
    /// into the cache. The collection may reserve more space to avoid frequent
    /// reallocations.
    ///
    /// # Arguments
    ///
    /// * `additional`: The number of new entries beyond the ones already
    /// contained in the cache for which space should be reserved.
    ///
    /// # Panics
    ///
    /// If the new allocation size overflows [usize].
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> = LruCache::new(1024);
    /// cache.insert("key".to_owned(), "value".to_owned()).unwrap();
    /// cache.reserve(10);
    /// assert!(cache.capacity() >= 11);
    /// ```
    pub fn reserve(&mut self, additional: usize) {
        let new_capacity = self.new_capacity(additional).unwrap();

        if self.capacity() < new_capacity {
            self.reallocate(new_capacity);
        }
    }

    /// Tries to reserve capacity for at least `additional` new entries to be
    /// inserted into the cache. The collection may reserve more space to avoid
    /// frequent reallocations.
    ///
    /// # Arguments
    ///
    /// * `additional`: The number of new entries beyond the ones already
    /// contained in the cache for which space should be reserved.
    ///
    /// # Errors
    ///
    /// If the capacity overflows, or the allocator reports an error, then an
    /// error is returned.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> = LruCache::new(1024);
    /// cache.insert("key".to_owned(), "value".to_owned()).unwrap();
    /// cache.try_reserve(10).expect("out of memory");
    /// assert!(cache.capacity() >= 11);
    /// ```
    pub fn try_reserve(&mut self, additional: usize)
            -> Result<(), TryReserveError> {
        let new_capacity = self.new_capacity(additional)?;

        if self.capacity() < new_capacity {
            self.try_reallocate(new_capacity)
        }
        else {
            Ok(())
        }
    }

    /// Shrinks the capacity of the cache with a lower bound. The capacity will
    /// remain at least as large as both the [length](LruCache::len) and the
    /// given lower bound while maintaining internal constraints of the hash
    /// table.
    ///
    /// If the capacity is less than the given lower bound, this method is
    /// no-op.
    ///
    /// # Arguments
    ///
    /// * `min_capacity`: A lower bound on the capacity after the operation.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> =
    ///     LruCache::with_capacity(1024, 10);
    /// assert!(cache.capacity() >= 10);
    /// cache.shrink_to(5);
    /// assert!(cache.capacity() >= 5);
    /// ```
    pub fn shrink_to(&mut self, min_capacity: usize) {
        let new_capacity = self.len().max(min_capacity);

        if self.capacity() > new_capacity {
            self.reallocate(new_capacity);
        }
    }

    /// Shrinks the capacity of the cache as much as possible. It will drop
    /// down as much as possible while maintaining internal constraints of the
    /// hash table.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache: LruCache<String, String> =
    ///     LruCache::with_capacity(1024, 10);
    /// cache.insert("key".to_owned(), "value".to_owned()).unwrap();
    /// cache.shrink_to_fit();
    /// assert!(cache.capacity() >= 1);
    /// ```
    pub fn shrink_to_fit(&mut self) {
        self.shrink_to(0)
    }

    /// Sets a new memory limit for this cache. If this is below the current
    /// size (see [LruCache::current_size]), the least-recently-used element
    /// will be repeatedly ejected until the limit is satisfied.
    ///
    /// Note that reducing the memory limit to a small fraction of the previous
    /// maximum may lead to large amounts of unused capacity in the underlying
    /// data structure. If this is a problem, use [LruCache::shrink_to] or
    /// [LruCache::shrink_to_fit] to avoid this.
    ///
    /// # Arguments
    ///
    /// * `max_size`: The new maximum number of bytes that the sum of the
    /// memory estimates of all entries may occupy.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.set_max_size(cache.current_size() - 1);
    ///
    /// assert_eq!(1, cache.len());
    /// assert!(cache.max_size() < 1024);
    /// ```
    pub fn set_max_size(&mut self, max_size: usize) {
        self.eject_to_target(max_size);
        self.max_size = max_size;
    }

    /// Sets the entry with the given key as most-recently-used, i.e. all other
    /// entries currently contained in the cached will be dropped before this
    /// one (unless others are touched/used afterwards). If there is no value
    /// associated with the given key, this method is no-op.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the entry to touch.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.touch(&"apple".to_owned());
    ///
    /// assert_eq!(Some(("lemon".to_owned(), "sour".to_owned())),
    ///     cache.remove_lru());
    /// ```
    pub fn touch<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        if let Some(entry) = self.get_mut_from_table(key) {
            let entry_ptr = EntryPtr::new(entry as *mut Entry<K, V>);
            self.touch_ptr(entry_ptr);
        }
    }

    /// Gets references to the key and value of the entry associated with the
    /// given key. If there is no entry for that key, `None` is returned.
    ///
    /// This method also marks the entry as most-recently-used (see
    /// [LruCache::touch]). If you do not want the usage history to be updated,
    /// use [LruCache::peek_entry] instead.
    ///
    /// The memory requirement of the key and value may not be changed.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the entry to get.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some((&"apple".to_owned(), &"sweet".to_owned())),
    ///     cache.get_entry("apple"));
    /// assert_eq!(Some(("lemon".to_owned(), "sour".to_owned())),
    ///     cache.remove_lru());
    /// ```
    pub fn get_entry<Q>(&mut self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        if let Some(entry) = self.get_mut_from_table(key) {
            let entry_ptr = EntryPtr::new(entry as *mut Entry<K, V>);
            self.touch_ptr(entry_ptr);
            let entry = unsafe { entry_ptr.get_extended() };
            Some(unsafe { (entry.key(), entry.value()) })
        }
        else {
            None
        }
    }

    /// Gets a reference to the value associated with the given key. If there
    /// is no value for that key, `None` is returned.
    ///
    /// This method also marks the value as most-recently-used (see
    /// [LruCache::touch]). If you do not want the usage history to be updated,
    /// use [LruCache::peek] instead.
    ///
    /// The memory requirement of the value may not be changed.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the value to get.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some(&"sweet".to_owned()), cache.get("apple"));
    /// assert_eq!(Some(("lemon".to_owned(), "sour".to_owned())),
    ///     cache.remove_lru());
    /// ```
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        self.get_entry(key).map(|(_, v)| v)
    }

    /// Gets references to the key and value of the entry associated with the
    /// given key. If there is no entry for that key, `None` is returned.
    ///
    /// This method does not mark the value as most-recently-used. If you want
    /// the usage history to be updated, use [LruCache::get_entry] instead.
    ///
    /// The memory requirement of the key and value may not be changed.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the entry to peek.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some((&"apple".to_owned(), &"sweet".to_owned())),
    ///     cache.peek_entry("apple"));
    /// assert_eq!(Some(("apple".to_owned(), "sweet".to_owned())),
    ///     cache.remove_lru());
    /// ```
    pub fn peek_entry<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        self.get_from_table(key).map(|e| unsafe { (e.key(), e.value()) })
    }

    /// Gets a reference to the value associated with the given key. If there
    /// is no value for that key, `None` is returned.
    ///
    /// This method does not mark the value as most-recently-used. If you want
    /// the usage history to be updated, use [LruCache::get] instead.
    ///
    /// The memory requirement of the value may not be changed.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the value to peek.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some(&"sweet".to_owned()), cache.peek("apple"));
    /// assert_eq!(Some(("apple".to_owned(), "sweet".to_owned())),
    ///     cache.remove_lru());
    /// ```
    pub fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        self.get_from_table(key).map(|e| unsafe { e.value() })
    }

    /// Indicates whether this cache contains an entry associated with the
    /// given key. If there is one, it is _not_ marked as most-recently-used.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the value to search.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    ///
    /// assert!(cache.contains("apple"));
    /// assert!(!cache.contains("banana"));
    /// ```
    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        let hash = make_hash::<Q, S>(&self.hash_builder, key);
        self.table.find(hash, equivalent_key(key)).is_some()
    }

    /// Removes the entry associated with the given key from this cache. If the
    /// cache does not contain the given key, `None` is returned.
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the value to remove.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.remove("apple");
    ///
    /// assert_eq!(0, cache.len());
    /// ```
    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        self.remove_from_table(key)
            .map(|entry| unsafe { self.remove_metadata(entry) })
    }

    /// Removes and returns the value associated with the given key from this
    /// cache. If there is no such value, `None` is returned.
    ///
    /// # Argument
    ///
    /// * `key`: The key of the value to remove.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Some("sour".to_owned()), cache.remove("lemon"));
    /// assert_eq!(1, cache.len());
    /// ```
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized
    {
        self.remove_entry(key).map(|(_, v)| v)
    }

    /// Retains only the elements which satisfy the predicate. In other words,
    /// removes all entries `(k, v)` such that `pred(&k, &v)` returns `false`.
    /// The elements are visited ordered from least-recently-used to
    /// most-recently-used.
    ///
    /// For all retained entries, i.e. those where `pred` returns `true`, the
    /// memory requirement of the key and value may not be changed.
    ///
    /// # Arguments
    ///
    /// * `pred`: A function which takes as input references to the key and
    /// value of an entry and decides whether it should remain in the map
    /// (`true`) or not (`false`).
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    /// cache.insert("banana".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.retain(|_, v| v.as_str() == "sweet");
    ///
    /// assert_eq!(2, cache.len());
    /// assert!(cache.get("apple").is_some());
    /// assert!(cache.get("lemon").is_none());
    /// assert!(cache.get("banana").is_some());
    /// ```
    pub fn retain<F>(&mut self, mut pred: F)
    where
        F: FnMut(&K, &V) -> bool
    {
        let mut tail = self.seal.get().prev;

        while tail != self.seal {
            unsafe {
                let entry = tail.get();
                let key = entry.key();

                if !pred(key, entry.value()) {
                    self.remove_entry(key);
                }

                tail = entry.prev;
            }
        }
    }
}

struct EntryTooLarge<K, V> {
    key: K,
    value: V,
    entry_size: usize,
    max_size: usize
}

impl<K, V> From<EntryTooLarge<K, V>> for InsertError<K, V> {
    fn from(data: EntryTooLarge<K, V>) -> InsertError<K, V> {
        InsertError::EntryTooLarge {
            key: data.key,
            value: data.value,
            entry_size: data.entry_size,
            max_size: data.max_size
        }
    }
}

impl<K, V> From<EntryTooLarge<K, V>> for TryInsertError<K, V> {
    fn from(data: EntryTooLarge<K, V>) -> TryInsertError<K, V> {
        TryInsertError::EntryTooLarge {
            key: data.key,
            value: data.value,
            entry_size: data.entry_size,
            max_size: data.max_size
        }
    }
}

impl<K, V, S> LruCache<K, V, S>
where
    K: Eq + Hash + MemSize,
    V: MemSize,
    S: BuildHasher
{
    fn prepare_insert(&mut self, key: K, value: V)
            -> Result<UnhingedEntry<K, V>, EntryTooLarge<K, V>> {
        let entry = UnhingedEntry::new(key, value);
        let entry_size = entry.size();

        if entry_size > self.max_size {
            let (key, value) = entry.into_key_value();

            Err(EntryTooLarge {
                key,
                value,
                entry_size,
                max_size: self.max_size
            })
        }
        else {
            Ok(entry)
        }
    }

    fn insert_unchecked(&mut self, entry: UnhingedEntry<K, V>, hash: u64) {
        let size = entry.size();
        let mut entry = Entry::new(entry, self.seal, self.seal.get().next);

        loop {
            match self.insert_into_table_with_hash(hash, entry) {
                Ok(entry_ptr) => {
                    self.current_size += size;
                    self.set_head(entry_ptr);
                    return;
                },
                Err(returned_entry) => {
                    entry = returned_entry;
                    self.reallocate((self.table.capacity() * 2).max(1));
                    
                    // The seal pointer stays constant through reallocation, so
                    // only entry.next has to be set.

                    entry.next = self.seal.get().next;
                }
            }
        }
    }

    /// Inserts a new entry into this cache. This is initially the
    /// most-recently-used entry. If there was an entry with the given key
    /// before, it is removed and its value returned. Otherwise, `None` is
    /// returned. If inserting this entry would violate the memory limit,
    /// the least-recently-used values are ejected from the cache until it
    /// fits.
    ///
    /// If you want to know before calling this method whether elements would
    /// be ejected, you can use [entry_size] to obtain the memory usage that
    /// would be assigned to the created entry and check using
    /// [LruCache::current_size] and [LruCache::max_size] whether it fits.
    ///
    /// # Arguments
    ///
    /// * `key`: The key by which the inserted entry will be identified.
    /// * `value`: The value to store in the inserted entry.
    ///
    /// # Errors
    ///
    /// Raises an [InsertError::EntryTooLarge] if the entry alone would already
    /// be too large to fit inside the cache's size limit. That is, even if all
    /// other entries were ejected, it still would not be able to be inserted.
    /// If this occurs, the entry was not inserted.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(2, cache.len());
    /// ```
    pub fn insert(&mut self, key: K, value: V)
            -> Result<Option<V>, InsertError<K, V>> {
        let entry = self.prepare_insert(key, value)?;

        // Deduplicate keys, make space

        let key = entry.key();
        let hash = make_insert_hash::<K, S>(&self.hash_builder, key);
        let result = self.table.remove_entry(hash, equivalent_key(key))
            .map(|e| unsafe { self.remove_metadata(e).1 });
        self.eject_to_target(self.max_size - entry.size());

        // Insert entry at head of list

        self.insert_unchecked(entry, hash);
        Ok(result)
    }

    /// Tries to insert a new entry into this cache. This is initially the
    /// most-recently-used entry. If there was an entry with the given key
    /// before or it does not fit within the memory requirement, an appropriate
    /// error is raised (see below).
    ///
    /// # Arguments
    ///
    /// * `key`: The key by which the inserted entry will be identified.
    /// * `value`: The value to store in the inserted entry.
    ///
    /// # Errors
    ///
    /// * Raises an [TryInsertError::EntryTooLarge] if the entry alone would
    /// already be too large to fit inside the cache's size limit.
    /// * Otherwise, raises a [TryInsertError::WouldEjectLru] if the entry does
    /// not fit within the remaining free memory of the cache, i.e. the
    /// difference between [LruCache::max_size] and [LruCache::current_size].
    /// * Otherwise, raises an [TryInsertError::OccupiedEntry] if there was
    /// already an entry with the given key.
    ///
    /// If any error was raised, the entry was not inserted into the cache.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// let result = cache.try_insert("apple".to_owned(), "sweet".to_owned());
    /// assert!(result.is_ok());
    /// let result = cache.try_insert("apple".to_owned(), "sour".to_owned());
    /// assert!(!result.is_ok());
    /// ```
    pub fn try_insert(&mut self, key: K, value: V)
            -> Result<(), TryInsertError<K, V>> {
        let entry = self.prepare_insert(key, value)?;

        // Check that the entry fits

        let free_memory = self.max_size - self.current_size;
        let entry_size = entry.size();

        if entry_size > free_memory {
            let (key, value) = entry.into_key_value();

            return Err(TryInsertError::WouldEjectLru {
                key,
                value,
                entry_size,
                free_memory
            })
        }

        // Check that the entry is not occupied

        let key = entry.key();
        let hash = make_insert_hash::<K, S>(&self.hash_builder, key);

        if self.table.find(hash, equivalent_key(key)).is_some() {
            let (key, value) = entry.into_key_value();

            return Err(TryInsertError::OccupiedEntry {
                key,
                value
            })
        }

        self.insert_unchecked(entry, hash);
        Ok(())
    }

    /// Applies a mutating function to the value associated with the given key.
    /// The result of that function is returned. If there is no value for the
    /// given key, `None` is returned, and the operation is never called.
    /// Otherwise, the entry is marked as most-recently-used by this method.
    ///
    /// Note that the operation may also change the size of the value. After it
    /// terminates, the internal sizes are updated and, if necessary,
    /// least-recently-used entries are ejected to restore the memory
    /// requirement. If the operation increases the size beyond the limit of
    /// this cache, an error is raised (see below).
    ///
    /// # Arguments
    ///
    /// * `key`: The key of the value to mutate.
    /// * `op`: An operation that takes as input a mutable reference to the
    /// value, mutates it, and returns the desired result. This is forwarded by
    /// this method to the caller.
    ///
    /// # Errors
    ///
    /// Raises an [MutateError::EntryTooLarge] if the operation expanded the
    /// value so much that the entry no longer fit inside the memory limit of
    /// the cache. If that is the case, the entry is removed and its parts
    /// returned in the error data.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple".to_owned(), "sweet".to_owned()).unwrap();
    /// cache.insert("lemon".to_owned(), "sour".to_owned()).unwrap();
    ///
    /// assert_eq!(Ok(Some(())),
    ///     cache.mutate("apple", |s| s.push_str(" and sour")));
    /// assert_eq!(Some(&"sweet and sour".to_owned()), cache.peek("apple"));
    /// ```
    pub fn mutate<Q, R, F>(&mut self, key: &Q, op: F)
        -> Result<Option<R>, MutateError<K, V>>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
        F: FnOnce(&mut V) -> R
    {
        let max_size = self.max_size;

        if let Some(entry) = self.get_mut_from_table(key) {
            let new_value_size;
            let result;
            let old_value_size;

            unsafe {
                old_value_size = entry.value().mem_size();
                result = op(entry.value_mut());
                new_value_size = entry.value().mem_size();
            }

            if new_value_size > old_value_size {
                // The operation was expanding; we must ensure it still fits.

                let diff = new_value_size - old_value_size;
                let new_entry_size = entry.size + diff;

                if new_entry_size > max_size {
                    // The entry is too large after the operation; eject it and
                    // raise according error.

                    let old_entry_size = entry.size;
                    let (key, value) = self.remove_entry(key).unwrap();

                    return Err(MutateError::EntryTooLarge {
                        key,
                        value,
                        old_entry_size,
                        new_entry_size,
                        max_size
                    });
                }

                entry.size = new_entry_size;
                let entry_ptr = EntryPtr::new(entry as *mut Entry<K, V>);
                self.current_size += diff;
                self.touch_ptr(entry_ptr);
                self.eject_to_target(max_size);
            }
            else {
                // The operation was non-expanding; everything is ok.

                let diff = old_value_size - new_value_size;
                entry.size -= diff;
                let entry_ptr = EntryPtr::new(entry as *mut Entry<K, V>);
                self.current_size -= diff;
                self.touch_ptr(entry_ptr);
            }

            Ok(Some(result))
        }
        else {
            Ok(None)
        }
    }
}

impl<K, V, S> IntoIterator for LruCache<K, V, S> {
    type Item = (K, V);
    type IntoIter = IntoIter<K, V, S>;

    fn into_iter(self) -> IntoIter<K, V, S> {
        IntoIter::new(self)
    }
}

impl<K, V, S> Clone for LruCache<K, V, S>
where
    K: Clone + Eq + Hash,
    V: Clone,
    S: BuildHasher + Clone
{
    fn clone(&self) -> LruCache<K, V, S> {
        let max_size = self.max_size;
        let capacity = self.capacity();
        let hash_builder = self.hash_builder.clone();
        let mut clone = LruCache::with_capacity_and_hasher(
            max_size, capacity, hash_builder);
        clone.current_size = self.current_size;
        let mut next = self.seal.get().prev;

        while next != self.seal {
            let entry = unsafe { next.get().clone() };
            next = entry.prev;
            clone.insert_untracked(entry);
        }

        clone
    }
}

impl<K, V, S> Drop for LruCache<K, V, S> {
    fn drop(&mut self) {
        for entry in self.table.drain() {
            unsafe { entry.drop() };
        }

        unsafe {
            self.seal.drop_seal();
        }
    }
}

impl<K: Debug, V: Debug, S> Debug for LruCache<K, V, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

// Since the LruCache contains raw pointers, it is not automatically marked as
// Send and Sync. We will provide manual implementations as well as arguments
// why that is ok.

// It is implicitly assumed that every LruCache only contains pointers to
// memory that belongs to the cache itself. So, if an LruCache is sent to
// another thread, that memory can now only be accessed by that thread. In
// other words, two LruCaches or anything related (e.g. iterators) can never
// access the same memory. Therefore, sending them is no issue.

unsafe impl<K: Send, V: Send, S: Send> Send for LruCache<K, V, S> { }

// If an immutable reference to an LruCache exists, there is simultaneously no
// mutable reference to the same cache. By design of the cache, any operations
// applied to that reference will allow no writing access to any of its memory.
// Those that yield any possibility of writing, such as cloning, are restricted
// to newly allocated memory. Therefore, sending references is no issue, and by
// definition of Sync, LruCache may implement it.

unsafe impl<K: Sync, V: Sync, S: Sync> Sync for LruCache<K, V, S> { }

#[cfg(test)]
mod tests {
    use std::hash::Hasher;
    use std::sync::{Arc, Mutex};
    use super::*;

    pub(crate) fn singleton_test_cache() -> LruCache<&'static str, &'static str> {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache
    }

    pub(crate) fn large_test_cache() -> LruCache<&'static str, &'static str> {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        cache.insert("ahoy", "mars").unwrap();
        cache.insert("hi", "venus").unwrap();
        cache.insert("good morning", "jupiter").unwrap();
        cache
    }

    #[test]
    fn cache_correctly_inserts_with_sufficient_capacity() {
        let mut cache = LruCache::new(1024);
    
        assert_eq!(0, cache.len());
        assert_eq!(0, cache.current_size());
        assert_eq!(1024, cache.max_size());
    
        assert_eq!(Ok(None),
            cache.insert("key1".to_owned(), "value1".to_owned()));
    
        let new_size = cache.current_size();
    
        assert_eq!(1, cache.len());
        assert!(new_size > 0);
        assert_eq!(Some(&"value1".to_owned()), cache.get("key1"));
    
        assert_eq!(Ok(None),
            cache.insert("key2".to_owned(), "value2".to_owned()));
    
        assert_eq!(2, cache.len());
        assert!(cache.current_size() > new_size);
        assert_eq!(Some(&"value1".to_owned()), cache.get("key1"));
        assert_eq!(Some(&"value2".to_owned()), cache.get("key2"));
    }
    
    #[test]
    fn cache_deduplicates_keys() {
        let mut cache = LruCache::new(1024);
    
        assert_eq!(Ok(None),
            cache.insert("key".to_owned(), "value".to_owned()));
    
        let expected_size = cache.current_size;
    
        assert_eq!(Ok(Some("value".to_owned())),
            cache.insert("key".to_owned(), "value".to_owned()));
        assert_eq!(expected_size, cache.current_size());
        assert_eq!(1, cache.len());
    }
    
    fn string_with_size(size: usize) -> String {
        let mut s = String::with_capacity(size);
    
        for _ in 0..size {
            s.push('0');
        }
    
        s
    }
    
    #[test]
    fn cache_ejects_lru_if_overflowing() {
        let mut cache = LruCache::new(2048);
    
        // On 256-bit arch, each entry requires an extra 289 bytes per entry
        // in addition to the size of the value in bytes (see test case
        // "entry_correctly_computes_size", value_str_bytes = 1).
        // On 16-bit arch, this would be 19 bytes.
        // The numbers in this test case accommodate anywhere from 15 to 696
        // extra bytes (676 * 3 + 15 = 2050, 676 * 2 + 696 = 2048).
        
        cache.insert("a".to_owned(), string_with_size(676)).unwrap();
        cache.insert("b".to_owned(), string_with_size(676)).unwrap();
        
        let expected_size = cache.current_size();
        
        cache.insert("c".to_owned(), string_with_size(676)).unwrap();
        
        assert_eq!(expected_size, cache.current_size());
        assert_eq!(2, cache.len());
        assert!(cache.peek("a").is_none());
        assert!(cache.peek("b").is_some());
        assert!(cache.peek("c").is_some());
        
        assert_eq!(Some("b"), cache.peek_lru().map(|(key, _)| key.as_str()));
        assert_eq!(Some("c"), cache.peek_mru().map(|(key, _)| key.as_str()));
    }
    
    #[test]
    fn getting_sets_most_recently_used() {
        let mut cache = LruCache::new(2048);
    
        // See the argument why the sizes are like this in test case
        // "cache_ejects_lru_if_overflowing".
    
        cache.insert("a".to_owned(), string_with_size(674)).unwrap();
        cache.insert("b".to_owned(), string_with_size(674)).unwrap();
        cache.get(&"a".to_owned());
        cache.insert("c".to_owned(), string_with_size(674)).unwrap();
    
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_some());
    
        assert_eq!("a", unsafe { cache.seal.get().prev.get().key() });
        assert_eq!("c", unsafe { cache.seal.get().next.get().key() });
    
        cache.get("a");
    
        assert_eq!("c", unsafe { cache.seal.get().prev.get().key() });
        assert_eq!("a", unsafe { cache.seal.get().next.get().key() });
    }

    #[test]
    fn empty_cache_has_no_lru_and_mru() {
        let cache = LruCache::<&str, &str>::new(1024);

        assert!(cache.peek_lru().is_none());
        assert!(cache.peek_mru().is_none());
    }

    #[test]
    fn get_lru_sets_most_recently_used() {
        let mut cache = LruCache::new(2048);

        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();

        let lru = cache.get_lru();

        assert_eq!(Some((&"hello", &"world")), lru);

        cache.set_max_size(cache.current_size() - 1);

        assert!(cache.contains("hello"));
        assert!(!cache.contains("greetings"));
    }
    
    #[test]
    fn peeking_does_not_set_most_recently_used() {
        let mut cache = LruCache::new(2048);
    
        cache.insert("a".to_owned(), string_with_size(674)).unwrap();
        cache.insert("b".to_owned(), string_with_size(674)).unwrap();
        cache.peek(&"a".to_owned());
        cache.insert("c".to_owned(), string_with_size(674)).unwrap();
    
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());

        cache.peek_entry(&"b".to_owned());
        cache.insert("d".to_owned(), string_with_size(674)).unwrap();

        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_some());
        assert!(cache.get("d").is_some());
    }
    
    #[test]
    fn cache_rejects_too_large_entry() {
        let mut cache = LruCache::new(256);
        let key = "This is a pretty long key, especially considering that \
            keys should normally be rather small to avoid long hashing times."
            .to_owned();
        let value = "Although the key alone has insufficient size, together \
            with this string it pushes pushes the total memory requirement of the \
            entry over the capacity of the cache.".to_owned();
    
        assert!(matches!(cache.insert(key, value),
            Err(InsertError::EntryTooLarge { .. })));
    }
    
    #[test]
    fn precisely_fitting_entry_does_not_eject_lru() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
        cache.insert("greetings".to_owned(), "moon".to_owned()).unwrap();
    
        let key = "ahoy".to_owned();
        let mut value = "mars".to_owned();
        value.shrink_to_fit();
        let required_size = cache.max_size() - cache.current_size();
        let additional_bytes = required_size - entry_size(&key, &value);
        let additional_data = vec![b's'; additional_bytes];
        value.push_str(&String::from_utf8(additional_data).unwrap());
        value.shrink_to_fit();
    
        assert_eq!(required_size, entry_size(&key, &value));
    
        cache.insert(key, value).unwrap();
    
        assert_eq!(3, cache.len());
    }

    #[test]
    fn auto_reallocation_doubles_capacity() {
        let mut cache = LruCache::with_capacity(4096, 10);
        let capacity = cache.capacity();

        for index in 0..capacity {
            cache.insert(index, 0).unwrap();
        }

        cache.insert(capacity, 0).unwrap();

        assert_eq!(2 * capacity, cache.capacity());
    }
    
    #[test]
    fn try_insert_works_as_insert_if_ok() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
        cache.try_insert("greetings".to_owned(), "moon".to_owned()).unwrap();
    
        assert_eq!(2, cache.len());
        assert_eq!("world".to_owned(), cache.remove_lru().unwrap().1);
        assert_eq!("moon".to_owned(), cache.remove_lru().unwrap().1);
    }
    
    #[test]
    fn try_insert_fails_on_duplication() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
        let result = cache.try_insert("hello".to_owned(), "moon".to_owned());
    
        assert!(matches!(result, Err(TryInsertError::OccupiedEntry { .. })));
        assert_eq!(1, cache.len());
        assert_eq!(&"world".to_owned(), cache.get("hello").unwrap());
    }
    
    #[test]
    fn try_insert_fails_if_eject_required() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
        cache.insert("greetings".to_owned(), "moon".to_owned()).unwrap();
        cache.set_max_size(cache.current_size());
        let result = cache.try_insert("ahoy".to_owned(), "mars".to_owned());
    
        assert!(matches!(result, Err(TryInsertError::WouldEjectLru { .. })));
        assert_eq!(2, cache.len());
        assert!(!cache.contains("ahoy"));
    }
    
    #[test]
    fn try_insert_fails_if_too_large() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
        let value = String::from_utf8(vec![b'0'; 1024]).unwrap();
        let result = cache.try_insert("key".to_owned(), value);
    
        assert!(matches!(result, Err(TryInsertError::EntryTooLarge { .. })));
        assert_eq!(1, cache.len());
        assert!(!cache.contains("key"));
    }
    
    #[test]
    fn removing_works() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        cache.insert("ahoy", "mars").unwrap();
    
        assert_eq!(Some(("hello", "world")), cache.remove_entry("hello"));
        assert_eq!(None, cache.remove("hello"));
    }

    #[test]
    fn removing_mru_works() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        cache.insert("ahoy", "mars").unwrap();
        cache.remove_mru();

        assert_eq!(2, cache.len());
        assert!(cache.contains("hello"));
        assert!(cache.contains("greetings"));
    }

    #[test]
    fn clearing_works() {
        let mut cache = large_test_cache();
        cache.clear();

        assert!(cache.is_empty());
        assert!(cache.peek_lru().is_none());
        assert!(cache.peek_mru().is_none());
        assert!(cache.iter().next().is_none());
    }
    
    #[test]
    fn retain_does_not_affect_empty_cache() {
        let mut cache: LruCache<u64, Vec<u8>> = LruCache::new(1024);
        cache.retain(|k, v| v.len() as u64 == *k);
    
        assert_eq!(0, cache.len());
    }
    
    #[test]
    fn retain_works_if_all_match() {
        let mut cache: LruCache<u64, Vec<u8>> = LruCache::new(1024);
        cache.insert(1, vec![0]).unwrap();
        cache.insert(5, vec![2, 3, 5, 7, 11]).unwrap();
        cache.insert(4, vec![1, 4, 9, 16]).unwrap();
        cache.retain(|k, v| v.len() as u64 == *k);
    
        assert_eq!(3, cache.len());
        assert!(cache.contains(&1));
        assert!(cache.contains(&4));
        assert!(cache.contains(&5));
    }
    
    #[test]
    fn retain_works_if_none_match() {
        let mut cache: LruCache<u64, Vec<u8>> = LruCache::new(1024);
        cache.insert(1, vec![0, 1]).unwrap();
        cache.insert(5, vec![2, 3, 5, 7, 11, 13]).unwrap();
        cache.insert(4, vec![1, 4, 9, 16, 25]).unwrap();
        cache.retain(|k, v| v.len() as u64 == *k);
    
        assert_eq!(0, cache.len());
        assert!(!cache.contains(&1));
        assert!(!cache.contains(&4));
        assert!(!cache.contains(&5));
    }
    
    #[test]
    fn retain_works_if_some_match() {
        let mut cache: LruCache<u64, Vec<u8>> = LruCache::new(1024);
        cache.insert(1, vec![0, 1]).unwrap();
        cache.insert(5, vec![2, 3, 5, 7, 11]).unwrap();
        cache.insert(4, vec![1, 4, 9, 16]).unwrap();
        cache.retain(|k, v| v.len() as u64 == *k);
    
        assert_eq!(2, cache.len());
        assert!(!cache.contains(&1));
        assert!(cache.contains(&4));
        assert!(cache.contains(&5));
    }

    #[test]
    fn retain_sets_lru_and_mru_if_necessary() {
        let mut cache = LruCache::new(1024);
        cache.insert(1, 0).unwrap();
        cache.insert(2, 0).unwrap();
        cache.insert(3, 0).unwrap();
        cache.insert(4, 0).unwrap();
        cache.retain(|&k, _| k != 1 && k != 4);

        assert_eq!(Some((&2, &0)), cache.peek_lru());
        assert_eq!(Some((&3, &0)), cache.peek_mru());
    }
    
    #[test]
    fn contains_works() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
    
        assert!(cache.contains("hello"));
        assert!(!cache.contains("greetings"));
    }
    
    #[test]
    fn increasing_max_size_keeps_all_elements() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        cache.set_max_size(2048);
    
        assert_eq!(2, cache.len());
        assert_eq!(2048, cache.max_size());
    }
    
    #[test]
    fn decreasing_max_size_below_current_size_drops_elements() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        cache.set_max_size(cache.current_size() - 1);
    
        assert_eq!(1, cache.len());
        assert!(cache.current_size() < cache.max_size());
        assert!(cache.max_size() < 1024);
    }
    
    #[test]
    fn cache_correctly_applies_mutation() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello".to_owned(), "world".to_owned()).unwrap();
        cache.insert("greetings".to_owned(), "moon".to_owned()).unwrap();
    
        let old_size = cache.current_size();
        let result = cache.mutate("greetings", |s| {
            s.push_str(", from 384400 km away");
            s.shrink_to_fit();
            s.len()
        });
    
        assert_eq!(Ok(Some(25)), result);
        assert_eq!(2, cache.len());
        assert_eq!(old_size + 21, cache.current_size());
        assert_eq!(Some(&"moon, from 384400 km away".to_owned()),
            cache.get("greetings"));
    }
    
    #[test]
    fn cache_rejects_too_expanding_mutation() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", vec![0u8; 32]).unwrap();
        cache.insert("greetings", vec![0u8; 32]).unwrap();
    
        let old_size = cache.current_size();
        let result = cache.mutate("hello", |v| {
            v.append(&mut vec![0u8; 1000]);
        });
    
        assert!(matches!(result, Err(MutateError::EntryTooLarge { .. })));
        assert_eq!(1, cache.len());
        assert!(cache.current_size() < old_size);
        assert_eq!(None, cache.get("hello"));
    }

    #[test]
    fn non_expanding_mutation_works() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", vec![0u8; 32]).unwrap();
        cache.insert("greetings", vec![0u8; 32]).unwrap();

        let old_size = cache.current_size();
        let result = cache.mutate("hello", |v| {
            *v = Vec::new();
        });

        assert!(matches!(result, Ok(Some(_))));
        assert!(cache.current_size() < old_size);
        assert_eq!(2, cache.len());
    }

    #[test]
    fn mutation_on_non_existent_element_is_never_called() {
        let mut cache = LruCache::<&str, &str>::new(1024);
        let result = cache.mutate("hello", |_| { panic!("mutation was called") });

        assert_eq!(Ok(None), result);
    }
    
    #[test]
    fn reserving_adds_capacity() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        let additional = cache.capacity() - cache.len() + 5;
        cache.reserve(additional);
    
        assert!(cache.capacity() >= additional + 2);
        assert_eq!(2, cache.len());
        assert_eq!(Some((&"hello", &"world")), cache.peek_lru());
        assert_eq!(Some((&"greetings", &"moon")), cache.peek_mru());
    
        let additional = additional + 10;
    
        assert!(cache.try_reserve(additional).is_ok());
        assert!(cache.capacity() >= additional + 2);
    }

    #[test]
    fn reserving_does_not_change_anything_when_capacity_is_not_exceeded() {
        let mut cache = LruCache::with_capacity(1024, 5);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        let capacity_before = cache.capacity();

        assert!(cache.try_reserve(3).is_ok());
        assert_eq!(capacity_before, cache.capacity());
    }
    
    #[test]
    fn reserving_fails_on_internal_overflow() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();

        let try_reserve_result = cache.try_reserve(usize::MAX);

        assert_eq!(Err(TryReserveError::CapacityOverflow), try_reserve_result);
    }

    #[test]
    fn reserving_fails_on_external_overflow() {
        // In this test, the number of entries does not overflow usize, but the
        // number of bytes does. Hence, lru-mem does not raise an error, but
        // hashbrown does.

        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        let additional = usize::MAX / mem::size_of::<Entry<&str, &str>>();

        let try_reserve_result = cache.try_reserve(additional);

        assert_eq!(Err(TryReserveError::CapacityOverflow), try_reserve_result);
    }
    
    #[test]
    fn shrinking_does_not_drop_below_len() {
        let mut cache = large_test_cache();
        cache.shrink_to(4);
    
        assert!(cache.capacity() >= 5);
        assert_eq!(5, cache.len());
    
        cache.insert("hey", "mercury").unwrap();
        cache.insert("what's up", "saturn").unwrap();
        cache.shrink_to_fit();
    
        assert!(cache.capacity() >= 7);
        assert_eq!(7, cache.len());
    }

    #[test]
    fn shrinking_large_cache_decreases_capacity() {
        let mut cache = LruCache::with_capacity(1024, 256);
        cache.shrink_to(64);

        assert!(cache.capacity() < 256);
        assert!(cache.capacity() >= 64);

        cache.insert("hey", "mercury").unwrap();
        cache.insert("what's up", "saturn").unwrap();
        cache.shrink_to_fit();

        assert!(cache.capacity() < 64);
        assert!(cache.capacity() >= 2);
    }

    #[test]
    fn cache_created_with_capacity_does_not_reallocate_before_capacity_is_reached() {
        let mut cache = LruCache::with_capacity(4096, 10);
        let capacity_before = cache.capacity();

        assert!(capacity_before >= 10);

        for i in 0..10 {
            cache.insert(i, "value").unwrap();
        }

        assert_eq!(capacity_before, cache.capacity());
    }

    struct MockHasher {
        hash_requests: Arc<Mutex<u32>>
    }

    impl Hasher for MockHasher {
        fn finish(&self) -> u64 {
            *self.hash_requests.lock().unwrap() += 1;
            0
        }

        fn write(&mut self, _bytes: &[u8]) { }
    }

    struct MockBuildHasher {
        hash_requests: Arc<Mutex<u32>>
    }

    impl BuildHasher for MockBuildHasher {
        type Hasher = MockHasher;

        fn build_hasher(&self) -> Self::Hasher {
            MockHasher {
                hash_requests: Arc::clone(&self.hash_requests)
            }
        }
    }

    #[test]
    fn cache_uses_given_hasher() {
        let build_hasher = MockBuildHasher {
            hash_requests: Arc::new(Mutex::new(0))
        };
        let mut cache = LruCache::with_hasher(1024, build_hasher);

        assert_eq!(0, *cache.hasher().hash_requests.lock().unwrap());

        cache.insert("hello", "world").unwrap();

        assert_eq!(1, *cache.hasher().hash_requests.lock().unwrap());

        cache.insert("greetings", "moon").unwrap();
        cache.insert("ahoy", "mars").unwrap();

        assert_eq!(3, *cache.hasher().hash_requests.lock().unwrap());
    }

    #[test]
    fn clone_creates_independent_cache() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
    
        let mut clone = cache.clone();
        clone.insert("ahoy", "mars").unwrap();

        cache.remove(&"hello");
        cache.touch(&"greetings");
    
        assert_eq!(1, cache.len());
        assert_eq!(None, cache.get(&"hello"));
        assert_eq!(Some(&"moon"), cache.get(&"greetings"));
        assert_eq!(None, cache.get(&"ahoy"));
    
        assert_eq!(3, clone.len());
        assert_eq!(Some(&"world"), clone.get(&"hello"));
        assert_eq!(Some(&"moon"), clone.get(&"greetings"));
        assert_eq!(Some(&"mars"), clone.get(&"ahoy"));
    
        assert!(clone.current_size() > cache.current_size());

        let cache_drained = cache.drain().collect::<Vec<_>>();
        let cache_drained_expected = vec![("greetings", "moon")];
        let clone_drained = clone.drain().collect::<Vec<_>>();
        let clone_drained_expected = vec![
            ("hello", "world"),
            ("greetings", "moon"),
            ("ahoy", "mars")
        ];

        assert_eq!(cache_drained_expected, cache_drained);
        assert_eq!(clone_drained_expected, clone_drained);
    }
    
    #[test]
    fn touching_in_singleton_works() {
        // Note: This weirdly specific test case isolates a previous bug.
    
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.touch("hello");
    
        let mut iter = cache.keys();
    
        assert_eq!(Some(&"hello"), iter.next());
        assert_eq!(None, iter.next());
    }

    #[test]
    fn empty_cache_formats_for_debug_correctly() {
        let cache = LruCache::<&str, &str>::new(1024);

        assert_eq!("{}", format!("{:?}", cache));
    }

    #[test]
    fn singleton_cache_formats_for_debug_correctly() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();

        assert_eq!("{\"hello\": \"world\"}", format!("{:?}", cache));
    }

    #[test]
    fn larger_cache_formats_for_debug_correctly() {
        let mut cache = LruCache::new(1024);
        cache.insert(2, 4).unwrap();
        cache.insert(3, 9).unwrap();
        cache.insert(5, 25).unwrap();
        cache.insert(7, 49).unwrap();
        cache.insert(11, 121).unwrap();
        cache.touch(&2);

        assert_eq!("{3: 9, 5: 25, 7: 49, 11: 121, 2: 4}", format!("{:?}", cache));
    }
}
