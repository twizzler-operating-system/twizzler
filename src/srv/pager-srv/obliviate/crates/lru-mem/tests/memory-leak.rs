use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use lru_mem::{HeapSize, LruCache};

#[derive(Debug)]
struct MockKey {
    id: u64,
    heap_size: usize,
    drop_counter: Rc<RefCell<usize>>
}

impl MockKey {
    fn new(id: u64, heap_size: usize, drop_counter: &Rc<RefCell<usize>>)
            -> MockKey {
        MockKey {
            id,
            heap_size,
            drop_counter: drop_counter.clone()
        }
    }
}

impl Drop for MockKey {
    fn drop(&mut self) {
        *self.drop_counter.borrow_mut() += 1;
    }
}

impl HeapSize for MockKey {
    fn heap_size(&self) -> usize {
        self.heap_size
    }
}

impl Hash for MockKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.id)
    }
}

impl PartialEq for MockKey {
    fn eq(&self, other: &MockKey) -> bool {
        self.id == other.id
    }
}

impl Eq for MockKey { }

#[derive(Debug)]
struct MockValue {
    heap_size: usize,
    drop_counter: Rc<RefCell<usize>>
}

impl MockValue {
    fn new(heap_size: usize, drop_counter: &Rc<RefCell<usize>>) -> MockValue {
        MockValue {
            heap_size,
            drop_counter: drop_counter.clone()
        }
    }

    fn set_heap_size(&mut self, new_heap_size: usize) {
        self.heap_size = new_heap_size;
    }
}

impl Drop for MockValue {
    fn drop(&mut self) {
        *self.drop_counter.borrow_mut() += 1;
    }
}

impl HeapSize for MockValue {
    fn heap_size(&self) -> usize {
        self.heap_size
    }
}


fn insert_mock_entry(cache: &mut LruCache<MockKey, MockValue>, id: u64,
        heap_size: usize, drop_counter: &Rc<RefCell<usize>>) {
    let key_heap_size = heap_size / 2;
    let key = MockKey::new(id, key_heap_size, drop_counter);
    let value = MockValue::new(heap_size - key_heap_size, drop_counter);
    cache.insert(key, value).unwrap();
}

fn make_cache_with_drop_counter(len: usize, id_start: u64,
        heap_size_per_entry: usize)
        -> (LruCache<MockKey, MockValue>, Rc<RefCell<usize>>) {
    let drop_counter = Rc::new(RefCell::new(0));
    let mut cache = LruCache::new(usize::MAX);

    for index in 0..len {
        let id = id_start + index as u64;
        insert_mock_entry(&mut cache, id, heap_size_per_entry, &drop_counter);
    }

    (cache, drop_counter)
}

#[test]
fn auto_reallocation_does_not_leak_entries() {
    const LEN: usize = 1024;

    let drop_counter = Rc::new(RefCell::new(0));
    let mut cache = LruCache::new(usize::MAX);

    for index in 0..LEN {
        insert_mock_entry(&mut cache, index as u64, 128, &drop_counter);
    }

    drop(cache);

    assert_eq!(LEN * 2, *drop_counter.borrow());
}

#[test]
fn manual_reallocation_does_not_leak_entries() {
    const LEN: usize = 32;
    const ITERATION_COUNT: usize = 4;

    let (mut cache, drop_counter) = make_cache_with_drop_counter(LEN, 0, 128);

    for iteration_index in 0..ITERATION_COUNT {
        cache.reserve(cache.capacity() * iteration_index);
        cache.shrink_to_fit();
    }

    drop(cache);

    assert_eq!(LEN * 2, *drop_counter.borrow());
}

#[test]
fn lru_removal_by_insert_does_not_leak_entries() {
    const LEN: usize = 1024;
    const ITERATION_COUNT: usize = 1024;

    let (mut cache, drop_counter) = make_cache_with_drop_counter(LEN, 0, 128);

    cache.set_max_size(cache.current_size());

    for index in 0..ITERATION_COUNT {
        insert_mock_entry(&mut cache, (LEN + index) as u64, 128, &drop_counter);
    }

    assert_eq!(LEN, cache.len());

    drop(cache);

    assert_eq!((LEN + ITERATION_COUNT) * 2, *drop_counter.borrow());
}

#[test]
fn lru_removal_by_decreasing_max_size_does_not_leak_entries() {
    const LEN: usize = 1024;
    const ITERATION_COUNT: u64 = 10;

    let (mut cache, drop_counter) = make_cache_with_drop_counter(LEN, 0, 128);

    for _ in 0..ITERATION_COUNT {
        cache.set_max_size(cache.current_size() / 2);
    }

    assert!(cache.len() < LEN);

    drop(cache);

    assert_eq!(LEN * 2, *drop_counter.borrow());
}

#[test]
fn lru_removal_by_mutating_does_not_leak_entries() {
    const LEN: usize = 1024;

    let (mut cache, drop_counter) = make_cache_with_drop_counter(LEN, 0, 64);

    cache.set_max_size(cache.current_size());

    for index in 0..(LEN / 2) {
        let key = MockKey::new(index as u64, 0, &drop_counter);
        cache.mutate(&key, |value| value.set_heap_size(128)).unwrap();
    }

    assert!(cache.len() < LEN);

    drop(cache);

    // temporary keys also count drops
    assert_eq!(LEN * 5 / 2, *drop_counter.borrow());
}

#[test]
fn clearing_does_not_leak_entries() {
    const LEN: usize = 1024;

    let (mut cache, drop_counter) = make_cache_with_drop_counter(LEN, 0, 128);

    cache.clear();

    assert_eq!(LEN * 2, *drop_counter.borrow());
}

#[test]
fn retain_does_not_leak_entries() {
    const LEN: usize = 1024;

    let (mut cache, drop_counter) = make_cache_with_drop_counter(LEN, 0, 128);

    cache.retain(|key, _| key.id % 2 == 0);

    // keys and values of half of the entries => 2 * 0.5 * LEN
    assert_eq!(LEN, *drop_counter.borrow());
}
