use criterion::Criterion;
use rand::Rng;

use crate::bencher_extensions::CacheBenchmarkGroup;

fn mutate_in_place(string: &mut String) {
    let old = string.pop().unwrap();
    let new = (b'0' + b'f' - old as u8) as char;

    string.push(new);
}

fn mutate_expanding(string: &mut String) {
    let old_capacity = string.capacity();

    while string.capacity() <= old_capacity {
        string.push('0')
    }
}

pub(crate) fn mutate_no_eject_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "mutate-no-eject");
    let mut rng = rand::thread_rng();

    for &size in crate::CONSTANT_TIME_SIZES {
        group.bench_with_capped_cache(|cache, keys| {
            let key_index = rng.gen_range(0..keys.len());
            cache.mutate(&keys[key_index], mutate_in_place).unwrap();
        }, size);
    }
}

pub(crate) fn mutate_eject_benchmark(c: &mut Criterion) {
    let mut group = crate::make_group(c, "mutate-eject");
    let mut rng = rand::thread_rng();

    for &size in crate::CONSTANT_TIME_SIZES {
        group.bench_with_refilled_capped_cache(|cache, keys| {
            let key_index = rng.gen_range(0..keys.len());
            let key = &keys[key_index];
            let lru_key = cache.peek_lru().unwrap().0.clone();
            cache.mutate(key, mutate_expanding).unwrap();
            [lru_key, key.clone()]
        }, size);
    }
}
