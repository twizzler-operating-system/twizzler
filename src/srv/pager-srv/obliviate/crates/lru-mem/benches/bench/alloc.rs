use criterion::Criterion;

use lru_mem::LruCache;

use crate::bencher_extensions::CacheBenchmarkGroup;

pub(crate) fn alloc_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "alloc");

    for &size in crate::LINEAR_TIME_SIZES {
        group.bench_with_reset_cache(|cache| {
            cache.reserve(cache.capacity());
        }, LruCache::shrink_to_fit, size);
    }
}
