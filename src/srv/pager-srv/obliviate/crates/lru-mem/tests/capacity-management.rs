use lru_mem::LruCache;

#[test]
fn capacity_does_not_grow_significantly_beyond_necessary() {
    let mut cache = LruCache::new(usize::MAX);

    for key_index in 0..4 {
        cache.insert(format!("{}", key_index), "value").unwrap();
    }

    for key_index in 4..4096 {
        cache.remove(&format!("{}", key_index - 2));
        cache.insert(format!("{}", key_index), "value").unwrap();
    }

    assert!(cache.capacity() < 128);
}
