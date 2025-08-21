use std::{ptr::addr_of, sync::atomic::AtomicU64};

use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{sys_map_ctrl, MapControlCmd, SyncFlags, SyncInfo},
};
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

    let release = AtomicU64::new(0);
    let release_ptr = addr_of!(release);

    let sync_info = SyncInfo {
        release: release_ptr,
        release_compare: 0,
        release_set: 1,
        durable: core::ptr::null(),
        flags: SyncFlags::DURABLE,
    };

    let sync_info_ptr = addr_of!(sync_info);

    sys_map_ctrl(
        map_write.base_ptr::<u8>(),
        MAX_SIZE,
        MapControlCmd::Sync(sync_info_ptr),
        0,
    )
    .unwrap();
    sys_map_ctrl(
        map_stable.base_ptr::<u8>(),
        MAX_SIZE,
        MapControlCmd::Update,
        0,
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

    use twizzler_rt_abi::bindings::twz_rt_malloc;

    #[bench]
    fn bench_rt_alloc(bench: &mut test::Bencher) {
        bench.iter(|| {
            let _ret = unsafe { twz_rt_malloc(32, 16, 0) };
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
