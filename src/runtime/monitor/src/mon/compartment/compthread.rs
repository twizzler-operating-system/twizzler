use twizzler_abi::{object::MAX_SIZE, upcall::UpcallFrame};
use twizzler_rt_abi::object::ObjID;

use crate::mon::{
    space::MapHandle,
    thread::{ManagedThread, DEFAULT_STACK_SIZE, STACK_SIZE_MIN_ALIGN},
};

pub(super) struct CompThread {
    stack_object: StackObject,
    thread: ManagedThread,
}

impl CompThread {
    /// Start a new thread using the given stack, in the provided security context instance, using
    /// the start function.
    pub fn new(
        stack: StackObject,
        instance: ObjID,
        start: impl FnOnce() + Send + 'static,
    ) -> miette::Result<Self> {
        todo!()
    }

    /// Get the entry frame for this thread into a given compartment.
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
    /// Make a new stack object from a given handle and stack size.
    pub fn new(handle: MapHandle, stack_size: usize) -> miette::Result<Self> {
        // Find the stack size, with max and min values, and correct alignment.
        let stack_size = std::cmp::max(std::cmp::min(stack_size, MAX_SIZE / 2), DEFAULT_STACK_SIZE)
            .next_multiple_of(STACK_SIZE_MIN_ALIGN);

        Ok(Self { handle, stack_size })
    }

    /// Get the start start address for the compartment.
    pub fn stack_comp_start(&self) -> usize {
        self.handle.addrs().start
    }

    /// Get the stack size.
    pub fn stack_size(&self) -> usize {
        self.stack_size
    }

    // This works for architectures where the stack grows down. If your architecture does not use a
    // downward-growing stack, implement this function differently.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    /// Get the initial stack pointer.
    pub fn initial_stack_ptr(&self) -> usize {
        self.stack_comp_start() + self.stack_size
    }
}
