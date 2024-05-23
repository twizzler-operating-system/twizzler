use dynlink::library::CtorInfo;
use miette::IntoDiagnostic;
use twizzler_abi::upcall::UpcallFrame;
use twizzler_runtime_api::ObjID;

use super::stack_object::{MainThreadReadyWaiter, StackObject};
use crate::threadman::{ManagedThreadRef, DEFAULT_STACK_SIZE};

pub(super) struct CompThread {
    stack_object: StackObject,
    thread: ManagedThreadRef,
}

impl CompThread {
    pub fn new(instance: ObjID, start: impl FnOnce() + Send + 'static) -> miette::Result<Self> {
        let thread = crate::threadman::start_managed_thread(start).into_diagnostic()?;
        Ok(Self {
            stack_object: StackObject::new(instance, DEFAULT_STACK_SIZE)?,
            thread,
        })
    }

    pub fn get_entry_frame(&self, ctx: ObjID, entry: usize, arg: usize) -> UpcallFrame {
        UpcallFrame::new_entry_frame(self.stack_object.initial_stack_ptr(), 0, ctx, entry, arg)
    }

    pub fn prep_stack_object(&mut self) -> miette::Result<MainThreadReadyWaiter> {
        Ok(MainThreadReadyWaiter {})
    }
}
