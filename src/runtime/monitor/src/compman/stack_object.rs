use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{BackingType, CreateTieFlags, CreateTieSpec, ObjectCreate, ObjectCreateFlags},
};
use twizzler_runtime_api::{MapFlags, ObjID};

use crate::{
    mapman::{safe_create_and_map_object, MapHandle},
    threadman::{DEFAULT_STACK_SIZE, STACK_SIZE_MIN_ALIGN},
};

// Layout: |---stack---|---init_data---|---TLS image---|
pub(crate) struct StackObject {
    handle: MapHandle,
    stack_size: usize,
    init_size: usize,
}

impl StackObject {
    pub fn new<T: Copy>(
        instance: ObjID,
        init_data: T,
        tls_align: usize,
        stack_size: usize,
    ) -> miette::Result<Self> {
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
        let stack_align = std::cmp::max(STACK_SIZE_MIN_ALIGN, core::mem::align_of::<T>());
        let stack_size = std::cmp::max(std::cmp::min(stack_size, MAX_SIZE / 2), DEFAULT_STACK_SIZE)
            .next_multiple_of(stack_align);
        // init size takes into account TLS alignment.
        let init_size = core::mem::size_of::<T>().next_multiple_of(tls_align);

        unsafe {
            // Write the init data.
            let stack_top = mh.monitor_data_null().add(stack_size);
            (stack_top as *mut T).write(init_data);
        }

        Ok(Self {
            handle: mh,
            stack_size,
            init_size,
        })
    }

    pub fn write_init_data<T>(&self, data: T) {
        unsafe {
            // Write the init data.
            let stack_top = self.handle.monitor_data_null().add(self.stack_size);
            (stack_top as *mut T).write(data);
        }
    }

    pub fn stack_comp_start(&self) -> usize {
        self.handle.addrs().start
    }

    pub fn stack_size(&self) -> usize {
        self.stack_size
    }

    pub fn init_data_comp_start(&self) -> usize {
        self.stack_comp_start() + self.stack_size()
    }

    pub fn init_data_size(&self) -> usize {
        self.init_size
    }
}
