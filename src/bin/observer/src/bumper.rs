use twizzler_object::{Object, ObjectInitFlags, ObjectInitError};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Bump {
    internal : Object<BumpInternal>,
}

struct BumpInternal {
    ids : AtomicU64,
}

impl Bump {
    fn new() -> Bump {
        let create = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Persistent,
            None,
            ObjectCreateFlags::empty(),
        );
    
        let vecid = twizzler_abi::syscall::sys_object_create(
            create,
            &[],
            &[],
        ).unwrap();
    
        let obj = Object::<BumpInternal>::init_id(
            vecid,
            Protections::WRITE | Protections::READ,
            ObjectInitFlags::empty(),
        ).unwrap();

        obj
    }
}