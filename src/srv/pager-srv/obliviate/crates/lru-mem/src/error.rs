use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};

/// An enumeration of the different errors that can occur when calling
/// [LruCache::insert](crate::LruCache::insert).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InsertError<K, V> {

    /// This error is raised if the amount of memory required to store an entry
    /// to be inserted is larger than the maximum of the cache.
    EntryTooLarge {

        /// The key of the entry which was too large.
        key: K,

        /// The value of the entry which was too large.
        value: V,

        /// The computed size requirement of the entry if it were in the cache
        /// in bytes.
        entry_size: usize,

        /// The maximum size of the cache in bytes.
        max_size: usize
    }
}

impl<K, V> Display for InsertError<K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            InsertError::EntryTooLarge { .. } =>
                write!(f, "entry does not fit in cache")
        }
    }
}

impl<K: Debug, V: Debug> Error for InsertError<K, V> { }

/// An enumeration of the different errors that can occur when calling
/// [LruCache::mutate](crate::LruCache::mutate).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutateError<K, V> {

    /// This error is raised if the memory requirement of an entry is raised
    /// above the maximum capacity of the cache in a mutation.
    EntryTooLarge {

        /// The key of the mutated entry.
        key: K,

        /// The mutated value.
        value: V,

        /// The size requirement of the entry before its mutation in bytes.
        old_entry_size: usize,

        /// The size requirement of the entry after its mutation in bytes, if
        /// it were in the cache.
        new_entry_size: usize,

        /// The maximum size of the cache in bytes.
        max_size: usize
    }
}

impl<K, V> Display for MutateError<K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            MutateError::EntryTooLarge { .. } =>
                write!(f, "modified entry does not fit in cache")
        }
    }
}

impl<K: Debug, V: Debug> Error for MutateError<K, V> { }

/// An enumeration of the different errors that can occur when calling
/// [LruCache::try_insert](crate::LruCache::try_insert).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TryInsertError<K, V> {

    /// This error is raised if the cache already contained an entry with a key
    /// equal to the given one.
    OccupiedEntry {

        /// The key of the entry to insert.
        key: K,

        /// The value of the entry to insert.
        value: V,
    },

    /// This error is raised if the cache cannot fit the given entry without
    /// ejecting the LRU element.
    WouldEjectLru {

        /// The key of the entry to insert.
        key: K,

        /// The value of the entry to insert.
        value: V,

        /// The computed size requirement of the entry if it were in the cache
        /// in bytes.
        entry_size: usize,

        /// The remaining free memory of the cache in bytes.
        free_memory: usize
    },

    /// This error is raised if the amount of memory required to store an entry
    /// to be inserted is larger than the maximum of the cache.
    EntryTooLarge {

        /// The key of the entry which was too large.
        key: K,

        /// The value of the entry which was too large.
        value: V,

        /// The computed size requirement of the entry if it were in the cache
        /// in bytes.
        entry_size: usize,

        /// The maximum size of the cache in bytes.
        max_size: usize
    }
}

impl<K, V> Display for TryInsertError<K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            TryInsertError::OccupiedEntry { .. } =>
                write!(f, "key already has associated entry"),
            TryInsertError::WouldEjectLru { .. } =>
                write!(f, "entry does not fit within remaining memory"),
            TryInsertError::EntryTooLarge { .. } =>
                write!(f, "entry does not fit in cache")
        }
    }
}

impl<K: Debug, V: Debug> Error for TryInsertError<K, V> { }

impl<K, V> TryInsertError<K, V> {

    /// Gets references to the key and value of the entry for which
    /// [LruCache::try_insert](crate::LruCache::try_insert) failed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple", "sweet").unwrap();
    /// let err = cache.try_insert("apple", "sour").unwrap_err();
    ///
    /// assert_eq!((&"apple", &"sour"), err.entry());
    /// ```
    pub fn entry(&self) -> (&K, &V) {
        match self {
            TryInsertError::OccupiedEntry { key, value, .. } => (key, value),
            TryInsertError::WouldEjectLru { key, value, .. } => (key, value),
            TryInsertError::EntryTooLarge { key, value, .. } => (key, value)
        }
    }

    /// Gets a reference to the key of the entry for which
    /// [LruCache::try_insert](crate::LruCache::try_insert) failed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple", "sweet").unwrap();
    /// let err = cache.try_insert("apple", "sour").unwrap_err();
    ///
    /// assert_eq!(&"apple", err.key());
    /// ```
    pub fn key(&self) -> &K {
        self.entry().0
    }

    /// Gets a reference to the value of the entry for which
    /// [LruCache::try_insert](crate::LruCache::try_insert) failed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple", "sweet").unwrap();
    /// let err = cache.try_insert("apple", "sour").unwrap_err();
    ///
    /// assert_eq!(&"sour", err.value());
    /// ```
    pub fn value(&self) -> &V {
        self.entry().1
    }

    /// Takes ownership of the error and returns the key and value of the entry
    /// for which [LruCache::try_insert](crate::LruCache::try_insert) failed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple", "sweet").unwrap();
    /// let err = cache.try_insert("apple", "sour").unwrap_err();
    ///
    /// assert_eq!(("apple", "sour"), err.into_entry());
    /// ```
    pub fn into_entry(self) -> (K, V) {
        match self {
            TryInsertError::OccupiedEntry { key, value, .. } => (key, value),
            TryInsertError::WouldEjectLru { key, value, .. } => (key, value),
            TryInsertError::EntryTooLarge { key, value, .. } => (key, value)
        }
    }

    /// Takes ownership of the error and returns the key of the entry for which
    /// [LruCache::try_insert](crate::LruCache::try_insert) failed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple", "sweet").unwrap();
    /// let err = cache.try_insert("apple", "sour").unwrap_err();
    ///
    /// assert_eq!("apple", err.into_key());
    /// ```
    pub fn into_key(self) -> K {
        self.into_entry().0
    }

    /// Takes ownership of the error and returns the value of the entry for
    /// which [LruCache::try_insert](crate::LruCache::try_insert) failed.
    ///
    /// # Example
    ///
    /// ```
    /// use lru_mem::LruCache;
    ///
    /// let mut cache = LruCache::new(1024);
    /// cache.insert("apple", "sweet").unwrap();
    /// let err = cache.try_insert("apple", "sour").unwrap_err();
    ///
    /// assert_eq!("sour", err.into_value());
    /// ```
    pub fn into_value(self) -> V {
        self.into_entry().1
    }
}
