#![allow(dead_code)]


#[cfg(kani)]
mod naming {
    use naming_core::{Entry, EntryType, ErrorKind, NameStore};

    use twizzler_minruntime;
    use twizzler_abi::syscall::{
        self, sys_object_create, BackingType, CreateTieSpec, LifetimeType, ObjectCreate, ObjectCreateFlags, Syscall,
    };

    fn raw_syscall_kani_stub(call: Syscall, args: &[u64]) -> (u64, u64) {

        // if core::intrinsics::unlikely(args.len() > 6) {
        //     twizzler_abi::print_err("too many arguments to raw_syscall");
        //     // crate::internal_abort();
        // }
        let a0 = *args.first().unwrap_or(&0u64);
        let a1 = *args.get(1).unwrap_or(&0u64);
        let mut a2 = *args.get(2).unwrap_or(&0u64);
        let a3 = *args.get(3).unwrap_or(&0u64);
        let a4 = *args.get(4).unwrap_or(&0u64);
        let a5 = *args.get(5).unwrap_or(&0u64);

        let mut num = call.num();
        //TODO: Skip actual inline assembly invcation and register inputs
        //TODO: Improve actual logic here

        (num,a2)
    }
 
    #[kani::proof]
    #[kani::stub(twizzler_abi::arch::syscall::raw_syscall,raw_syscall_kani_stub)]
    #[kani::stub(twizzler_rt_abi::bindings::twz_rt_map_object, twizzler_minruntime::runtime::syms::twz_rt_map_object)]
    #[kani::unwind(20)]
    fn test_single_put_then_get() {
        let store = NameStore::new();
        
        let num: u128 = kani::any();
        let session = store.root_session();
        assert_eq!(session.put("foo", EntryType::Object(num)), Ok(()));
        assert_eq!(
            session.get("foo"),
            Entry::try_new("foo", EntryType::Object(num))
        );
    }


    #[kani::proof]
    #[kani::stub(twizzler_abi::arch::syscall::raw_syscall,raw_syscall_kani_stub)]
    #[kani::stub(twizzler_rt_abi::bindings::twz_rt_map_object, twizzler_minruntime::runtime::syms::twz_rt_map_object)]
    #[kani::unwind(20)]
    fn test_e() {
        let store = NameStore::new();
        
        let e: EntryType = kani::any();
        let session = store.root_session();
        assert_eq!(session.put("foo", e), Ok(()));
        assert_eq!(
            session.get("foo"),
            Entry::try_new("foo", e)
        );
    }

#[kani::proof]
#[kani::stub(twizzler_abi::arch::syscall::raw_syscall, raw_syscall_kani_stub)]
#[kani::stub(twizzler_rt_abi::bindings::twz_rt_map_object, twizzler_minruntime::runtime::syms::twz_rt_map_object)]
    #[kani::unwind(20)]
    fn kani_traverse_namespace_nested_1() {
        let store = NameStore::new();
        let mut session = store.root_session();

        assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
        assert_eq!(session.put("foo", EntryType::Object(0)), Ok(()));

        assert_eq!(session.change_namespace("namespace"), Ok(()));

        let obj_id: u128 = kani::any();
        assert_eq!(session.put("foo", EntryType::Object(obj_id)), Ok(()));

        assert_eq!(
            session.get("foo"),
            Entry::try_new("foo", EntryType::Object(obj_id))
        );
        assert_eq!(
            session.get("/foo"),
            Entry::try_new("foo", EntryType::Object(0))
        );
    }

}

use naming_core::{Entry, EntryType, ErrorKind, NameStore};

fn test_single_put_then_get() {
    println!("doing test_single_put_then_get");
    let store = NameStore::new();

    let session = store.root_session();
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
}

fn test_multi_put_then_get() {
    println!("doing test_multi_put_then_get");

    let store = NameStore::new();

    let session = store.root_session();

    for i in 0..10 {
        for j in 0..10 {
            assert_eq!(session.put(j.to_string(), EntryType::Object(i)), Ok(()));
        }

        for j in 0..10 {
            assert_eq!(
                session.get(j.to_string()),
                Entry::try_new(j.to_string(), EntryType::Object(i))
            );
        }
    }
}

fn put_namespace() {
    println!("doing put_namespace");

    let store = NameStore::new();

    let session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("namespace/foo", EntryType::Object(2)), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/foo"),
        Entry::try_new("foo", EntryType::Object(2))
    );
    assert_eq!(
        session.get("namespace"),
        Entry::try_new("namespace", EntryType::Namespace)
    );
}

fn namespace_nested() {
    println!("doing put_namespace_nested");

    let store = NameStore::new();

    let session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(
        session.put("namespace/namespace", EntryType::Namespace),
        Ok(())
    );
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("namespace/foo", EntryType::Object(2)), Ok(()));
    assert_eq!(
        session.put("namespace/namespace/foo", EntryType::Object(3)),
        Ok(())
    );

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/foo"),
        Entry::try_new("foo", EntryType::Object(2))
    );
    assert_eq!(
        session.get("namespace/namespace/foo"),
        Entry::try_new("foo", EntryType::Object(3))
    );
    assert_eq!(
        session.get("namespace"),
        Entry::try_new("namespace", EntryType::Namespace)
    );
    assert_eq!(
        session.get("namespace/namespace"),
        Entry::try_new("namespace", EntryType::Namespace)
    );
}

fn traverse_namespace_nested_1() {
    println!("doing test_traverse_namespace");

    let store = NameStore::new();

    let mut session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(0)), Ok(()));

    assert_eq!(session.change_namespace("namespace"), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("/foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
}

fn traverse_namespace_nested_2() {
    println!("doing test_traverse_namespace");

    let store = NameStore::new();

    let mut session = store.root_session();

    assert_eq!(session.put("namespace", EntryType::Namespace), Ok(()));
    assert_eq!(session.put("foo", EntryType::Object(0)), Ok(()));
    assert_eq!(session.put("baz", EntryType::Object(0)), Ok(()));

    assert_eq!(
        session.change_namespace("foo"),
        Err(ErrorKind::NotNamespace)
    );
    assert_eq!(session.change_namespace("bux"), Err(ErrorKind::NotFound));
    assert_eq!(session.change_namespace("namespace"), Ok(()));

    assert_eq!(session.put("foo", EntryType::Object(1)), Ok(()));
    assert_eq!(session.put("baz", EntryType::Object(1)), Ok(()));

    assert_eq!(
        session.change_namespace("namespace"),
        Err(ErrorKind::NotFound)
    );

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );

    assert_eq!(session.change_namespace("."), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );

    assert_eq!(session.change_namespace(".."), Ok(()));

    assert_eq!(
        session.get("foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("namespace/foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("namespace/../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("namespace/../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/../foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/../baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/../namespace/foo"),
        Entry::try_new("foo", EntryType::Object(1))
    );
    assert_eq!(
        session.get("/../namespace/../namespace/baz"),
        Entry::try_new("baz", EntryType::Object(1))
    );
    assert_eq!(
        session.get("/.././.././foo"),
        Entry::try_new("foo", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/.././.././baz"),
        Entry::try_new("baz", EntryType::Object(0))
    );
    assert_eq!(
        session.get("/../namespace/namespace/../../foo"),
        Err(ErrorKind::NotFound)
    );
    assert_eq!(
        session.get("/../namespace/namespace/../../baz"),
        Err(ErrorKind::NotFound)
    );
}

fn main() {
    test_single_put_then_get();
    test_multi_put_then_get();
    put_namespace();
    namespace_nested();
    traverse_namespace_nested_1();
    traverse_namespace_nested_2();
}
