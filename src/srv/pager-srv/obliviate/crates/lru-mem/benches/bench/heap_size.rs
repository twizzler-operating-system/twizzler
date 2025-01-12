use std::iter;

use criterion::{black_box, Criterion};

use lru_mem::HeapSize;

use crate::get_id;

fn heap_size_benchmark_with<T, F>(make_value: F, sizes: &[usize], group_name: &str,
    c: &mut Criterion)
where
    T: HeapSize,
    F: Fn() -> T
{
    let mut group = crate::make_group(c, group_name);

    for &size in sizes {
        let value = iter::repeat_with(&make_value).take(size).collect::<Vec<_>>();
        let id = get_id(size);
        group.bench_function(id, |b| b.iter(|| {
            let heap_size = value.heap_size();
            black_box(heap_size);
        }));
    }
}

pub(crate) fn heap_size_benchmark(c: &mut Criterion) {
    heap_size_benchmark_with(|| 0u8, crate::CONSTANT_TIME_SIZES, "heap_size/Vec/u8", c);
    heap_size_benchmark_with(
        || Box::new(0u8), crate::CONSTANT_TIME_SIZES, "heap_size/Vec/Box", c);
    heap_size_benchmark_with(
        || String::from("hello"), crate::LINEAR_TIME_SIZES, "heap_size/Vec/String", c);
    heap_size_benchmark_with(
        || [Box::new(0u8), Box::new(1u8)], crate::CONSTANT_TIME_SIZES, "heap_size/Vec/Array", c);
}
