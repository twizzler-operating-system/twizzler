use criterion::Criterion;
use lru_mem::LruCache;

use crate::bencher_extensions::CacheBenchmarkGroup;

pub(crate) fn retain_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "retain");

    for &size in crate::LINEAR_TIME_SIZES {
        group.bench_with_reset_cache(|cache| {
            cache.retain(|key, _| key.chars().last().unwrap() != '7');
        }, LruCache::clear, size);
    }
}
