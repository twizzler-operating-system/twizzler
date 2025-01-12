use criterion::Criterion;

use crate::bencher_extensions::CacheBenchmarkGroup;

pub(crate) fn iter_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "iter");

    for &size in crate::LINEAR_TIME_SIZES {
        group.bench_with_capped_cache(|cache, _| {
            for entry in cache.iter() {
                criterion::black_box(entry);
            }
        }, size);
    }
}
