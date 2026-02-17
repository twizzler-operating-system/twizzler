use twizzler_rt_abi::object::MapFlags;

use crate::object::{Object, ObjectBuilder, RawObject};

#[test]
fn test_stable_read() {
    let obj = ObjectBuilder::default().build(42u64).unwrap();

    let map_stable = Object::<u64>::map(obj.id(), MapFlags::READ | MapFlags::INDIRECT).unwrap();
    let map_write = Object::<u64>::map(obj.id(), MapFlags::READ | MapFlags::WRITE).unwrap();

    unsafe {
        let value = map_stable.base_ptr::<u64>().read_volatile();
        assert_eq!(value, 42);

        map_write.base_mut_ptr::<u64>().write_volatile(128u64);
        let value = map_stable.base_ptr::<u64>().read_volatile();
        assert_eq!(value, 42);
        let value = map_write.base_ptr::<u64>().read_volatile();
        assert_eq!(value, 128);
    }

    map_write
        .handle()
        .cmd(
            twizzler_rt_abi::object::ObjectCmd::Sync,
            core::ptr::null_mut::<()>(),
        )
        .unwrap();
    map_stable
        .handle()
        .cmd(
            twizzler_rt_abi::object::ObjectCmd::Update,
            core::ptr::null_mut::<()>(),
        )
        .unwrap();

    unsafe {
        let value = map_stable.base_ptr::<u64>().read_volatile();
        assert_eq!(value, 128);
        let value = map_write.base_ptr::<u64>().read_volatile();
        assert_eq!(value, 128);
    }
}

#[cfg(test)]
mod tester {
    use std::time::Instant;

    use twizzler_rt_abi::bindings::{twz_rt_dealloc, twz_rt_get_monotonic_time, twz_rt_malloc};

    //#[bench]
    #[allow(dead_code)]
    fn bench_rt_alloc(bench: &mut test::Bencher) {
        let mut tracker = Vec::with_capacity(100000);
        bench.iter(|| {
            let _ret = unsafe { twz_rt_malloc(32, 16, 0) };
            let ret = core::hint::black_box(_ret);
            tracker.push(ret);
        });

        for r in tracker {
            unsafe { twz_rt_dealloc(r, 32, 16, 0) };
        }
    }

    #[bench]
    fn bench_rt_monotime(bench: &mut test::Bencher) {
        bench.iter(|| {
            let _ret = unsafe { twz_rt_get_monotonic_time() };
            core::hint::black_box(_ret);
        });
    }

    #[bench]
    fn bench1000_instant_now(bench: &mut test::Bencher) {
        bench.iter(|| {
            for _ in 0..1000 {
                let _ret = Instant::now();
                core::hint::black_box(_ret);
            }
        });
    }
}
