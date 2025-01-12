use lru_mem::LruCache;

fn test_cache_with_many_accesses<F>(intermediate_op: F)
where
    F: Fn(&mut LruCache<i32, i32>, i32)
{
    let mut cache = LruCache::new(1024);
    cache.insert(0, 0).unwrap();
    let entry_size = cache.current_size();

    for i in 1..=1000 {
        cache.insert(i, i).unwrap();

        for j in 0..=(i / 2) {
            cache.touch(&(j * 2));
        }

        intermediate_op(&mut cache, i);
    }

    let mut found_even = false;

    for i in 0..=1000 {
        let contained = cache.contains(&i);

        if i % 2 == 0 {
            if found_even {
                assert!(contained,
                    "cache did not contain even number {} but contains \
                        lower even number", i);
            }
            else if contained {
                found_even = true;
            }
        }
        else {
            assert!(!contained, "cache contained odd number {}", i);
        }
    }

    assert_eq!(entry_size * cache.len(), cache.current_size());
    assert!(cache.max_size() - cache.current_size() < entry_size);
}

#[test]
fn cache_works_with_many_accesses() {
    test_cache_with_many_accesses(|_, _| { })
}

#[test]
fn cache_works_with_many_reallocations() {
    test_cache_with_many_accesses(|cache, i| {
        match i % 10 {
            2 => {
                if i > 10 {
                    cache.shrink_to(cache.capacity() - 10)
                }
            },
            4 => cache.reserve(200),
            6 => cache.shrink_to_fit(),
            8 => cache.try_reserve(120).unwrap(),
            _ => { }
        }
    })
}
