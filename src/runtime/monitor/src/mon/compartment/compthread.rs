use dynlink::{compartment::MONITOR_COMPARTMENT_ID, context::Context};
use miette::IntoDiagnostic;
use twizzler_abi::{object::MAX_SIZE, upcall::UpcallFrame};
use twizzler_rt_abi::object::ObjID;

use crate::mon::{
    space::{MapHandle, Space},
    thread::{ManagedThread, ThreadMgr, DEFAULT_STACK_SIZE, STACK_SIZE_MIN_ALIGN},
};

#[allow(dead_code)]
pub(super) struct CompThread {
    pub(crate) stack_object: StackObject,
    pub(crate) thread: ManagedThread,
}

impl CompThread {
    /// Start a new thread using the given stack, in the provided security context instance, using
    /// the start function.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        space: &mut Space,
        tmgr: &mut ThreadMgr,
        dynlink: &mut Context,
        stack: StackObject,
        instance: ObjID,
        main_thread_comp: Option<ObjID>,
        entry: usize,
        arg: usize,
    ) -> miette::Result<Self> {
        let frame = stack.get_entry_frame(instance, entry, arg);
        let start = move || {
            twizzler_abi::syscall::sys_sctx_attach(instance).unwrap();
            unsafe { twizzler_abi::syscall::sys_thread_resume_from_upcall(&frame) };
        };
        let mon = dynlink.get_compartment_mut(MONITOR_COMPARTMENT_ID).unwrap();
        let mt = tmgr
            .start_thread(space, mon, Box::new(start), main_thread_comp)
            .into_diagnostic()?;
        Ok(Self {
            stack_object: stack,
            thread: mt,
        })
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
        let stack_size = stack_size
            .clamp(DEFAULT_STACK_SIZE, MAX_SIZE / 2)
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

    /// Get the entry frame for this thread into a given compartment.
    pub fn get_entry_frame(&self, ctx: ObjID, entry: usize, arg: usize) -> UpcallFrame {
        UpcallFrame::new_entry_frame(
            self.initial_stack_ptr(),
            self.stack_size(),
            0,
            ctx,
            entry,
            arg,
        )
    }
}
