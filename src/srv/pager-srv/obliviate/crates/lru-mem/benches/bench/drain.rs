use criterion::{black_box, Criterion};

use crate::bencher_extensions::CacheBenchmarkGroup;

pub(crate) fn drain_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "drain");

    for &size in crate::LINEAR_TIME_SIZES {
        group.bench_with_reset_cache(|cache| {
            for entry in cache.drain() {
                black_box(entry);
            }
        }, |_| { }, size);
    }
}
