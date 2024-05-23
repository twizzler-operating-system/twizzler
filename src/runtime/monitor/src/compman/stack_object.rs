use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{BackingType, CreateTieFlags, CreateTieSpec, ObjectCreate, ObjectCreateFlags},
};
use twizzler_runtime_api::{MapFlags, ObjID};

use crate::{
    mapman::{safe_create_and_map_object, MapHandle},
    threadman::{ManagedThreadRef, DEFAULT_STACK_SIZE, STACK_SIZE_MIN_ALIGN},
};

pub(crate) struct StackObject {
    handle: MapHandle,
    stack_size: usize,
}

impl StackObject {
    pub fn new(instance: ObjID, stack_size: usize) -> miette::Result<Self> {
        let cs = ObjectCreate::new(
            BackingType::Normal,
            twizzler_abi::syscall::LifetimeType::Volatile,
            Some(instance),
            ObjectCreateFlags::empty(),
        );
        let mh = safe_create_and_map_object(
            cs,
            &[],
            &[CreateTieSpec::new(instance, CreateTieFlags::empty())],
            MapFlags::READ | MapFlags::WRITE,
        )?;

        // Find the stack size, with max and min values, and correct alignment.
        let stack_size = std::cmp::max(std::cmp::min(stack_size, MAX_SIZE / 2), DEFAULT_STACK_SIZE)
            .next_multiple_of(STACK_SIZE_MIN_ALIGN);

        Ok(Self {
            handle: mh,
            stack_size,
        })
    }

    pub fn stack_comp_start(&self) -> usize {
        self.handle.addrs().start
    }

    pub fn stack_size(&self) -> usize {
        self.stack_size
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    pub fn initial_stack_ptr(&self) -> usize {
        self.stack_comp_start() + self.stack_size
    }
}
