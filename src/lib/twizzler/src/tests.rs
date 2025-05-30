use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{sys_map_ctrl, MapControlCmd, SyncFlags},
};
use twizzler_rt_abi::object::MapFlags;

use crate::object::{Object, ObjectBuilder, RawObject, TypedObject};

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
    sys_map_ctrl(
        map_write.base_ptr::<u8>(),
        MAX_SIZE,
        MapControlCmd::Sync(SyncFlags::empty()),
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
