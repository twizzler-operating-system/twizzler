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
mod adjacent {
    use std::time::{Duration, Instant};

    use twizzler_abi::{
        arch::SLOTS,
        object::Protections,
        syscall::{
            sys_object_create, sys_object_ctrl, sys_object_read_map, BackingType, DeleteFlags,
            LifetimeType, ObjectControlCmd, ObjectCreate, ObjectCreateFlags,
        },
    };
    use twizzler_rt_abi::object::{
        twz_rt_map_adjacent_objects, MapFlags, ObjID, ObjectHandle, MAX_SIZE, NULLPAGE_SIZE,
    };

    const RW: MapFlags = MapFlags::READ.union(MapFlags::WRITE);

    fn create(n: usize) -> Vec<ObjID> {
        let spec = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
            Protections::all(),
        );
        (0..n)
            .map(|_| sys_object_create(spec, &[], &[]).unwrap())
            .collect()
    }

    fn delete(ids: &[ObjID]) {
        for id in ids {
            let _ = sys_object_ctrl(*id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0);
        }
    }

    fn map(ids: &[ObjID]) -> Vec<ObjectHandle> {
        let objs: Vec<_> = ids.iter().map(|id| (*id, RW)).collect();
        let mut handles = vec![None; ids.len()];
        twz_rt_map_adjacent_objects(&objs, &mut handles).unwrap();
        let handles: Vec<_> = handles.into_iter().map(Option::unwrap).collect();
        let base = handles[0].start() as usize;
        for (i, h) in handles.iter().enumerate() {
            assert_eq!(h.id(), ids[i]);
            assert_eq!(h.start() as usize, base + i * MAX_SIZE);
        }
        handles
    }

    fn word(base: usize, i: usize) -> *mut u64 {
        (base + i * MAX_SIZE + NULLPAGE_SIZE) as *mut u64
    }

    fn holds(slot: usize, id: ObjID) -> bool {
        sys_object_read_map(None, slot).is_ok_and(|m| m.id == id)
    }

    #[test]
    fn test_map_adjacent() {
        for n in [1, 2, 4, 16] {
            let ids = create(n);
            let handles = map(&ids);
            let base = handles[0].start() as usize;
            for i in 0..n {
                unsafe { word(base, i).write_volatile(0x1000 + i as u64) };
            }
            for i in 0..n {
                assert_eq!(unsafe { word(base, i).read_volatile() }, 0x1000 + i as u64);
            }
            drop(handles);

            // The monitor unmaps in the background.
            let deadline = Instant::now() + Duration::from_secs(10);
            while (0..n).any(|i| holds(base / MAX_SIZE + i, ids[i])) {
                assert!(Instant::now() < deadline, "run of {} not unmapped", n);
                std::thread::sleep(Duration::from_millis(1));
            }

            let handles = map(&ids);
            let base = handles[0].start() as usize;
            for i in 0..n {
                assert_eq!(unsafe { word(base, i).read_volatile() }, 0x1000 + i as u64);
            }
            drop(handles);
            delete(&ids);
        }
    }

    fn wait_unmapped(slot: usize, id: ObjID) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while holds(slot, id) {
            assert!(Instant::now() < deadline, "slot {} not unmapped", slot);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn test_map_adjacent_overlapping() {
        let ids = create(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        let ab = map(&[a, b]);
        let bc = map(&[b, c]);
        let single = twizzler_rt_abi::object::twz_rt_map_object(b, RW).unwrap();
        let ptr = |h: &ObjectHandle| unsafe { h.start().add(NULLPAGE_SIZE).cast::<u64>() };
        let all = [&ab[0], &ab[1], &bc[0], &bc[1], &single];
        for (i, h) in all.iter().enumerate() {
            unsafe { ptr(h).add(i + 1).write_volatile(i as u64) };
        }

        let [ab0, ab1] = <[ObjectHandle; 2]>::try_from(ab).unwrap();
        let [bc0, bc1] = <[ObjectHandle; 2]>::try_from(bc).unwrap();
        let slot = |h: &ObjectHandle| h.start() as usize / MAX_SIZE;
        let (bc0_slot, ab0_slot) = (slot(&bc0), slot(&ab0));
        drop(bc0);
        wait_unmapped(bc0_slot, b);
        for (i, h) in [&ab0, &ab1, &bc1, &single].iter().enumerate() {
            unsafe { ptr(h).add(10 + i).write_volatile(i as u64) };
        }
        for h in [&ab1, &single] {
            assert_eq!(unsafe { ptr(h).add(2).read_volatile() }, 1);
            assert_eq!(unsafe { ptr(h).add(3).read_volatile() }, 2);
            assert_eq!(unsafe { ptr(h).add(5).read_volatile() }, 4);
        }
        drop(single);
        drop(ab0);
        wait_unmapped(ab0_slot, a);
        unsafe { ptr(&ab1).add(20).write_volatile(1) };
        unsafe { ptr(&bc1).add(20).write_volatile(2) };
        drop(ab1);
        drop(bc1);
        delete(&ids);
    }

    #[test]
    fn test_map_adjacent_fails_whole() {
        let ids = create(3);
        let bad = ObjID::new(0xdead_beef_dead_beef_dead_beef_dead_beef);
        let objs = [(ids[0], RW), (ids[1], RW), (bad, RW), (ids[2], RW)];
        let mut handles = [None, None, None, None];
        assert!(twz_rt_map_adjacent_objects(&objs, &mut handles).is_err());
        assert!(handles.iter().all(Option::is_none));
        for slot in 0..SLOTS {
            assert!(
                !ids.iter().any(|id| holds(slot, *id)),
                "slot {} left mapped",
                slot
            );
        }
        delete(&ids);
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
