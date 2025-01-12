use criterion::Criterion;
use rand::Rng;

use crate::bencher_extensions::CacheBenchmarkGroup;

pub(crate) fn get_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "get");
    let mut rng = rand::thread_rng();

    for &size in crate::CONSTANT_TIME_SIZES {
        group.bench_with_capped_cache(|cache, keys| {
            let key_index = rng.gen_range(0..keys.len());
            cache.get(&keys[key_index]);
        }, size);
    }
}
