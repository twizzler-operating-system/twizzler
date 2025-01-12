use std::time::Duration;
use criterion::{BenchmarkGroup, Criterion};
use criterion::measurement::WallTime;

mod alloc;
mod clone;
mod drain;
mod get;
mod heap_size;
mod insert;
mod iter;
mod mutate;
mod peek;
mod remove;
mod retain;
mod bencher_extensions;

const KIBI: usize = 1024;
const MEBI: usize = KIBI * KIBI;

pub(crate) fn get_id(size: usize) -> String {
    if size >= MEBI {
        format!("{}M", size / MEBI)
    }
    else if size >= KIBI {
        format!("{}K", size / KIBI)
    }
    else {
        format!("{}", size)
    }
}

pub(crate) fn make_group<'criterion>(c: &'criterion mut Criterion, name: &str)
        -> BenchmarkGroup<'criterion, WallTime> {
    const BENCH_DURATION: Duration = Duration::from_secs(15);
    const SAMPLE_SIZE: usize = 100;

    let mut group = c.benchmark_group(name);
    group.sample_size(SAMPLE_SIZE).measurement_time(BENCH_DURATION);
    group
}

const LINEAR_TIME_SIZES: &'static [usize] = &[
    64,
    1024,
    16 * 1024,
    256 * 1024
];

const CONSTANT_TIME_SIZES: &'static [usize] = &[
    1024,
    16 * 1024,
    256 * 1024,
    4 * 1024 * 1024
];

criterion::criterion_group!(benches,
    alloc::alloc_benchmark,
    clone::clone_benchmark,
    drain::drain_benchmark,
    get::get_benchmark,
    heap_size::heap_size_benchmark,
    insert::insert_no_eject_benchmark,
    insert::insert_eject_benchmark,
    iter::iter_benchmark,
    mutate::mutate_no_eject_benchmark,
    mutate::mutate_eject_benchmark,
    peek::peek_benchmark,
    remove::remove_benchmark,
    retain::retain_benchmark,
);

criterion::criterion_main!(benches);
