use twizzler_abi::{object::MAX_SIZE, upcall::UpcallFrame};
use twizzler_runtime_api::ObjID;

use crate::mon::{
    space::MapHandle,
    thread::{ManagedThread, DEFAULT_STACK_SIZE, STACK_SIZE_MIN_ALIGN},
};

pub(super) struct CompThread {
    stack_object: StackObject,
    thread: ManagedThread,
}

impl CompThread {
    pub fn new(
        stack: StackObject,
        instance: ObjID,
        start: impl FnOnce() + Send + 'static,
    ) -> miette::Result<Self> {
        todo!()
    }

    pub fn get_entry_frame(&self, ctx: ObjID, entry: usize, arg: usize) -> UpcallFrame {
        UpcallFrame::new_entry_frame(
            self.stack_object.initial_stack_ptr(),
            self.stack_object.stack_size(),
            0,
            ctx,
            entry,
            arg,
        )
    }
}

pub(crate) struct StackObject {
    handle: MapHandle,
    stack_size: usize,
}

impl StackObject {
    pub fn new(handle: MapHandle, stack_size: usize) -> miette::Result<Self> {
        // Find the stack size, with max and min values, and correct alignment.
        let stack_size = std::cmp::max(std::cmp::min(stack_size, MAX_SIZE / 2), DEFAULT_STACK_SIZE)
            .next_multiple_of(STACK_SIZE_MIN_ALIGN);

        Ok(Self { handle, stack_size })
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
