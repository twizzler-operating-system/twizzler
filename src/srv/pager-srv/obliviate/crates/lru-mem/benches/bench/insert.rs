use criterion::Criterion;
use lru_mem::LruCache;

use crate::bencher_extensions::CacheBenchmarkGroup;

#[inline]
fn insert_and_increment(cache: &mut LruCache<String, String>, key_idx: &mut u64) {
    cache.insert(format!("{:012x}", key_idx), String::new()).unwrap();
    *key_idx += 1;
}

pub(crate) fn insert_no_eject_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "insert-no-eject");

    for &size in crate::CONSTANT_TIME_SIZES {
        let min_size = size * 7 / 8;
        let mut key_idx: u64 = 0;

        group.bench_with_depleted_cache(
            |cache| insert_and_increment(cache, &mut key_idx), min_size, size);
    }
}

pub(crate) fn insert_eject_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "insert-eject");

    for &size in crate::CONSTANT_TIME_SIZES {
        let mut key_idx: u64 = 0;

        group.bench_with_capped_cache(
            |cache, _| insert_and_increment(cache, &mut key_idx), size);
    }
}
