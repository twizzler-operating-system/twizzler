use crate::LruCache;
use crate::entry::EntryPtr;

use std::iter::FusedIterator;
use std::marker::PhantomData;

/// An iterator over references to the entries of an [LruCache] ordered from
/// least- to most-recently-used. This is obtained by calling [LruCache::iter].
pub struct Iter<'a, K, V> {
    next: EntryPtr<K, V>,
    next_back: EntryPtr<K, V>,
    lifetime: PhantomData<&'a ()>
}

impl<'a, K, V> Iter<'a, K, V> {
    pub(crate) fn new<S>(cache: &LruCache<K, V, S>) -> Iter<K, V> {
        if cache.is_empty() {
            Iter {
                next: unsafe { EntryPtr::null() },
                next_back: unsafe { EntryPtr::null() },
                lifetime: PhantomData
            }
        }
        else {
            Iter {
                next: cache.seal.get().prev,
                next_back: cache.seal.get().next,
                lifetime: PhantomData
            }
        }
    }
}

impl<'a, K: 'a, V: 'a> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        if self.next.is_null() {
            None
        }
        else {
            let entry = unsafe { self.next.get_extended() };

            if self.next == self.next_back {
                self.next = unsafe { EntryPtr::null() };
            }
            else {
                self.next = entry.prev;
            }

            unsafe { Some((entry.key(), entry.value())) }
        }
    }
}

impl<'a, K: 'a, V: 'a> DoubleEndedIterator for Iter<'a, K, V> {
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        if self.next.is_null() {
            None
        }
        else {
            let entry = unsafe { self.next_back.get_extended() };

            if self.next_back == self.next {
                self.next = unsafe { EntryPtr::null() };
            }
            else {
                self.next_back = entry.next;
            }

            unsafe { Some((entry.key(), entry.value())) }
        }
    }
}

impl<'a, K: 'a, V: 'a> FusedIterator for Iter<'a, K, V> { }

/// An iterator over references to the keys of an [LruCache] ordered from
/// least- to most-recently-used. This is obtained by calling [LruCache::keys].
pub struct Keys<'a, K, V> {
    iter: Iter<'a, K, V>
}

impl<'a, K, V> Keys<'a, K, V> {
    pub(crate) fn new<S>(cache: &'a LruCache<K, V, S>) -> Keys<'a, K, V> {
        Keys {
            iter: Iter::new(cache)
        }
    }
}

impl<'a, K: 'a, V: 'a> Iterator for Keys<'a, K, V> {
    type Item = &'a K;

    fn next(&mut self) -> Option<&'a K> {
        self.iter.next().map(|(k, _)| k)
    }
}

impl<'a, K: 'a, V: 'a> DoubleEndedIterator for Keys<'a, K, V> {
    fn next_back(&mut self) -> Option<&'a K> {
        self.iter.next_back().map(|(k, _)| k)
    }
}

impl<'a, K: 'a, V: 'a> FusedIterator for Keys<'a, K, V> { }

/// An iterator over references to the values of an [LruCache] ordered from
/// least- to most-recently-used. This is obtained by calling
/// [LruCache::values].
pub struct Values<'a, K, V> {
    iter: Iter<'a, K, V>
}

impl<'a, K, V> Values<'a, K, V> {
    pub(crate) fn new<S>(cache: &'a LruCache<K, V, S>) -> Values<'a, K, V> {
        Values {
            iter: Iter::new(cache)
        }
    }
}

impl<'a, K: 'a, V: 'a> Iterator for Values<'a, K, V> {
    type Item = &'a V;

    fn next(&mut self) -> Option<&'a V> {
        self.iter.next().map(|(_, v)| v)
    }
}

impl<'a, K: 'a, V: 'a> DoubleEndedIterator for Values<'a, K, V> {
    fn next_back(&mut self) -> Option<&'a V> {
        self.iter.next_back().map(|(_, v)| v)
    }
}

impl<'a, K: 'a, V: 'a> FusedIterator for Values<'a, K, V> { }

struct TakingIterator<K, V> {
    next: EntryPtr<K, V>,
    next_back: EntryPtr<K, V>,
}

impl<K, V> TakingIterator<K, V> {
    fn new<S>(cache: &LruCache<K, V, S>) -> TakingIterator<K, V> {
        if cache.is_empty() {
            TakingIterator {
                next: unsafe { EntryPtr::null() },
                next_back: unsafe { EntryPtr::null() }
            }
        }
        else {
            TakingIterator {
                next: cache.seal.get().prev,
                next_back: cache.seal.get().next
            }
        }
    }
}

impl<K, V> Iterator for TakingIterator<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        if self.next.is_null() {
            None
        }
        else {
            unsafe {
                let entry = self.next.read();

                if self.next == self.next_back {
                    self.next = EntryPtr::null();
                }
                else {
                    self.next = entry.prev;
                }

                Some(entry.into_key_value())
            }
        }
    }
}

impl<K, V> DoubleEndedIterator for TakingIterator<K, V> {
    fn next_back(&mut self) -> Option<(K, V)> {
        if self.next.is_null() {
            None
        }
        else {
            unsafe {
                let entry = self.next_back.read();

                if self.next_back == self.next {
                    self.next = EntryPtr::null();
                }
                else {
                    self.next_back = entry.next;
                }

                Some(entry.into_key_value())
            }
        }
    }
}

/// An iterator that drains key-value-pairs from an [LruCache] ordered from
/// least- to most-recently-used. This is obtained by calling
/// [LruCache::drain].
pub struct Drain<'a, K, V, S> {
    iterator: TakingIterator<K, V>,
    cache: &'a mut LruCache<K, V, S>
}

impl<'a, K, V, S> Drain<'a, K, V, S> {
    pub(crate) fn new(cache: &'a mut LruCache<K, V, S>) -> Drain<'a, K, V, S> {
        Drain {
            iterator: TakingIterator::new(cache),
            cache
        }
    }
}

impl<'a, K, V, S> Iterator for Drain<'a, K, V, S> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.iterator.next()
    }
}

impl<'a, K, V, S> DoubleEndedIterator for Drain<'a, K, V, S> {
    fn next_back(&mut self) -> Option<(K, V)> {
        self.iterator.next_back()
    }
}

impl<'a, K, V, S> Drop for Drain<'a, K, V, S> {
    fn drop(&mut self) {
        // Drop all allocated memory of the remaining elements.

        for _ in self.by_ref() { }

        // Set the cache as empty.

        self.cache.seal.get_mut().next = self.cache.seal;
        self.cache.seal.get_mut().prev = self.cache.seal;

        self.cache.current_size = 0;
        self.cache.table.clear_no_drop();
    }
}

impl<'a, K, V, S> FusedIterator for Drain<'a, K, V, S> { }

/// An iterator that takes ownership of an [LruCache] and iterates over its
/// entries as key-value-pairs ordered from least- to most-recently-used. This
/// is obtained by calling [IntoIterator::into_iter] on the cache.
pub struct IntoIter<K, V, S> {
    iterator: TakingIterator<K, V>,
    cache: LruCache<K, V, S>
}

impl<K, V, S> IntoIter<K, V, S> {
    pub(crate) fn new(cache: LruCache<K, V, S>) -> IntoIter<K, V, S> {
        IntoIter {
            iterator: TakingIterator::new(&cache),
            cache
        }
    }
}

impl<K, V, S> Iterator for IntoIter<K, V, S> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.iterator.next()
    }
}

impl<K, V, S> DoubleEndedIterator for IntoIter<K, V, S> {
    fn next_back(&mut self) -> Option<(K, V)> {
        self.iterator.next_back()
    }
}

impl<K, V, S> Drop for IntoIter<K, V, S> {
    fn drop(&mut self) {
        // Drop all allocated memory of the remaining elements.
        for _ in self.by_ref() { }

        // Clear items from the cache without dropping their memory.
        self.cache.table.clear_no_drop();
    }
}

/// An iterator that takes ownership of an [LruCache] and iterates over its
/// keys ordered from least- to most-recently-used. This is obtained by calling
/// [LruCache::into_keys].
pub struct IntoKeys<K, V, S> {
    into_iter: IntoIter<K, V, S>
}

impl<K, V, S> IntoKeys<K, V, S> {
    pub(crate) fn new(cache: LruCache<K, V, S>) -> IntoKeys<K, V, S> {
        IntoKeys {
            into_iter: IntoIter::new(cache)
        }
    }
}

impl<K, V, S> Iterator for IntoKeys<K, V, S>  {
    type Item = K;

    fn next(&mut self) -> Option<K> {
        self.into_iter.next().map(|(k, _)| k)
    }
}

impl<K, V, S> DoubleEndedIterator for IntoKeys<K, V, S> {
    fn next_back(&mut self) -> Option<K> {
        self.into_iter.next_back().map(|(k, _)| k)
    }
}

/// An iterator that takes ownership of an [LruCache] and iterates over its
/// values ordered from least- to most-recently-used. This is obtained by
/// calling [LruCache::into_values].
pub struct IntoValues<K, V, S> {
    into_iter: IntoIter<K, V, S>
}

impl<K, V, S> IntoValues<K, V, S> {
    pub(crate) fn new(cache: LruCache<K, V, S>) -> IntoValues<K, V, S> {
        IntoValues {
            into_iter: IntoIter::new(cache)
        }
    }
}

impl<K, V, S> Iterator for IntoValues<K, V, S>  {
    type Item = V;

    fn next(&mut self) -> Option<V> {
        self.into_iter.next().map(|(_, v)| v)
    }
}

impl<K, V, S> DoubleEndedIterator for IntoValues<K, V, S> {
    fn next_back(&mut self) -> Option<V> {
        self.into_iter.next_back().map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {

    use std::sync::{Arc, Mutex};

    use crate::{HeapSize, LruCache};
    use crate::tests::{large_test_cache, singleton_test_cache};

    #[test]
    fn iter_works_for_larger_cache() {
        let cache = large_test_cache();
        let mut iter = cache.iter();

        assert_eq!(Some((&"hello", &"world")), iter.next());
        assert_eq!(Some((&"good morning", &"jupiter")), iter.next_back());
        assert_eq!(Some((&"hi", &"venus")), iter.next_back());
        assert_eq!(Some((&"ahoy", &"mars")), iter.next_back());
        assert_eq!(Some((&"greetings", &"moon")), iter.next());
        assert_eq!(None, iter.next_back());
        assert_eq!(None, iter.next());
    }

    #[test]
    fn forward_iter_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut iter = cache.iter();

        assert_eq!(Some((&"hello", &"world")), iter.next());
        assert_eq!(None, iter.next());
    }

    #[test]
    fn backward_iter_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut iter = cache.iter();

        assert_eq!(Some((&"hello", &"world")), iter.next_back());
        assert_eq!(None, iter.next_back());
    }

    #[test]
    fn forward_keys_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut keys = cache.keys();

        assert_eq!(Some(&"hello"), keys.next());
        assert_eq!(None, keys.next());
    }

    #[test]
    fn backward_keys_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut keys = cache.keys();

        assert_eq!(Some(&"hello"), keys.next_back());
        assert_eq!(None, keys.next_back());
    }

    #[test]
    fn forward_values_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut values = cache.values();

        assert_eq!(Some(&"world"), values.next());
        assert_eq!(None, values.next());
    }

    #[test]
    fn backward_values_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut values = cache.values();

        assert_eq!(Some(&"world"), values.next_back());
        assert_eq!(None, values.next_back());
    }

    #[test]
    fn separated_iters_zip_to_pair_iter() {
        let cache = large_test_cache();
        let pair_iter_collected = cache.iter().collect::<Vec<_>>();
        let zip_iter_collected = cache.keys()
            .zip(cache.values())
            .collect::<Vec<_>>();

        assert_eq!(pair_iter_collected, zip_iter_collected);
    }

    #[test]
    fn separated_into_iters_zip_to_pair_into_iter() {
        let cache = large_test_cache();
        let pair_iter_collected =
            cache.clone().into_iter().collect::<Vec<_>>();
        let zip_iter_collected = cache.clone().into_keys()
            .zip(cache.into_values())
            .collect::<Vec<_>>();

        assert_eq!(pair_iter_collected, zip_iter_collected);
    }

    #[test]
    fn drain_clears_cache() {
        let mut cache = LruCache::new(1024);
        cache.insert("hello", "world").unwrap();
        cache.insert("greetings", "moon").unwrap();
        cache.drain().next();

        assert_eq!(0, cache.len());
        assert_eq!(0, cache.current_size());
        assert!(cache.peek_lru().is_none());
        assert!(cache.peek_mru().is_none());
        assert!(cache.get("hello").is_none());
    }

    fn test_owning_iterator<I>(mut iter: I)
    where
        I: Iterator<Item = (&'static str, &'static str)> + DoubleEndedIterator
    {
        assert_eq!(Some(("hello", "world")), iter.next());
        assert_eq!(Some(("good morning", "jupiter")), iter.next_back());
        assert_eq!(Some(("hi", "venus")), iter.next_back());
        assert_eq!(Some(("ahoy", "mars")), iter.next_back());
        assert_eq!(Some(("greetings", "moon")), iter.next());
        assert_eq!(None, iter.next_back());
        assert_eq!(None, iter.next());
    }

    #[test]
    fn drain_returns_entries() {
        let mut cache = large_test_cache();
        let drain = cache.drain();
        test_owning_iterator(drain);
    }

    #[test]
    fn into_iter_returns_entries() {
        let cache = large_test_cache();
        let into_iter = cache.into_iter();
        test_owning_iterator(into_iter);
    }

    #[test]
    fn forward_into_iter_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut into_iter = cache.into_iter();

        assert_eq!(Some(("hello", "world")), into_iter.next());
        assert_eq!(None, into_iter.next());
    }

    #[test]
    fn backward_into_iter_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut into_iter = cache.into_iter();

        assert_eq!(Some(("hello", "world")), into_iter.next_back());
        assert_eq!(None, into_iter.next_back());
    }

    #[test]
    fn forward_into_keys_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut into_keys = cache.into_keys();

        assert_eq!(Some("hello"), into_keys.next());
        assert_eq!(None, into_keys.next());
    }

    #[test]
    fn backward_into_keys_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut into_keys = cache.into_keys();

        assert_eq!(Some("hello"), into_keys.next_back());
        assert_eq!(None, into_keys.next_back());
    }

    #[test]
    fn forward_into_values_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut into_values = cache.into_values();

        assert_eq!(Some("world"), into_values.next());
        assert_eq!(None, into_values.next());
    }

    #[test]
    fn backward_into_values_works_for_singleton_cache() {
        let cache = singleton_test_cache();
        let mut into_values = cache.into_values();

        assert_eq!(Some("world"), into_values.next_back());
        assert_eq!(None, into_values.next_back());
    }

    #[derive(Debug)]
    struct DropObserver {
        drop_counter: Arc<Mutex<u32>>
    }

    impl HeapSize for DropObserver {
        fn heap_size(&self) -> usize {
            0
        }
    }

    impl Drop for DropObserver {
        fn drop(&mut self) {
            *self.drop_counter.lock().unwrap() += 1;
        }
    }

    fn setup_cache_with_drop_counter() -> (LruCache<i32, DropObserver>, Arc<Mutex<u32>>) {
        let drop_counter = Arc::new(Mutex::new(0));
        let mut cache = LruCache::new(1024);
        cache.insert(0, DropObserver { drop_counter: Arc::clone(&drop_counter) })
            .unwrap();
        cache.insert(1, DropObserver { drop_counter: Arc::clone(&drop_counter) })
            .unwrap();

        (cache, drop_counter)
    }

    #[test]
    fn into_iter_drops_remaining_elements_if_dropped() {
        let (cache, drop_counter) = setup_cache_with_drop_counter();
        let mut into_iter = cache.into_iter();
        into_iter.next();

        assert_eq!(1, *drop_counter.lock().unwrap());

        drop(into_iter);

        assert_eq!(2, *drop_counter.lock().unwrap());
    }

    #[test]
    fn into_keys_drops_remaining_elements_if_dropped() {
        let (cache, drop_counter) = setup_cache_with_drop_counter();
        let mut into_keys = cache.into_keys();
        into_keys.next();

        assert_eq!(1, *drop_counter.lock().unwrap());

        drop(into_keys);

        assert_eq!(2, *drop_counter.lock().unwrap());
    }

    #[test]
    fn into_values_drops_remaining_elements_if_dropped() {
        let (cache, drop_counter) = setup_cache_with_drop_counter();
        let mut into_values = cache.into_values();
        into_values.next();

        assert_eq!(1, *drop_counter.lock().unwrap());

        drop(into_values);

        assert_eq!(2, *drop_counter.lock().unwrap());
    }

    #[test]
    fn drain_drops_remaining_elements_if_dropped() {
        let (mut cache, drop_counter) = setup_cache_with_drop_counter();
        let mut drain = cache.drain();
        drain.next();

        assert_eq!(1, *drop_counter.lock().unwrap());

        drop(drain);

        assert_eq!(2, *drop_counter.lock().unwrap());
        assert!(cache.is_empty());
    }

    fn assert_is_empty<T, I>(mut iterator: I)
    where
        I: Iterator<Item = T> + DoubleEndedIterator
    {
        assert!(iterator.next().is_none());
        assert!(iterator.next_back().is_none());
    }

    #[test]
    fn empty_cache_builds_empty_iterators() {
        let mut cache: LruCache<&str, &str> = LruCache::new(1024);

        assert_is_empty(cache.iter());
        assert_is_empty(cache.keys());
        assert_is_empty(cache.values());
        assert_is_empty(cache.drain());
        assert_is_empty(cache.clone().into_iter());
        assert_is_empty(cache.clone().into_keys());
        assert_is_empty(cache.into_values());
    }
}
