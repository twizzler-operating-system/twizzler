use dynlink::{compartment::MONITOR_COMPARTMENT_ID, context::Context};
use miette::IntoDiagnostic;
use twizzler_abi::{
    object::MAX_SIZE,
    upcall::{ResumeFlags, UpcallFrame},
};
use twizzler_rt_abi::object::ObjID;

use crate::mon::{
    space::MapHandle,
    thread::{EntryArgs, ManagedThread, ThreadMgr, DEFAULT_STACK_SIZE, STACK_SIZE_MIN_ALIGN},
};

#[allow(dead_code)]
pub struct CompThread {
    pub(crate) stack_object: StackObject,
    pub(crate) thread: ManagedThread,
}

impl CompThread {
    /// Start a new thread using the given stack, in the provided security context instance, using
    /// the start function.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tmgr: &mut ThreadMgr,
        dynlink: &mut Context,
        stack: StackObject,
        instance: ObjID,
        main_thread_comp: Option<ObjID>,
        entry: usize,
        arg: usize,
        suspend_on_start: bool,
    ) -> miette::Result<Self> {
        // The old closure captured an `UpcallFrame` (3264 B) and was itself a never-freed
        // `Box<dyn FnOnce()>`; both are gone. It also carried a `tracing::trace!` of entry latency,
        // which cannot survive without a capture and is dropped deliberately.
        let args = EntryArgs {
            instance,
            stack_ptr: stack.initial_stack_ptr(),
            stack_size: stack.stack_size(),
            thread_ptr: 0,
            entry,
            arg,
            suspend: suspend_on_start,
        };
        let mon = dynlink.get_compartment_mut(MONITOR_COMPARTMENT_ID).unwrap();
        let mt = tmgr
            .start_thread(mon, comp_main_entry, args, main_thread_comp, instance)
            .into_diagnostic()?;
        // Name it here because nothing else will: the runtime's `set_name` only fires if the
        // program calls it, and a compartment's main thread never does. Notes are where thread
        // names live, so this is the same channel `set_name` and the kernel's kthread names use.
        let _ = twizzler_abi::syscall::sys_object_add_note(mt.id, b"main");
        Ok(Self {
            stack_object: stack,
            thread: mt,
        })
    }
}

/// Entry point for a compartment's main thread. Plain function, no closure, no allocation.
///
/// `args` points at the base of this thread's own super stack. Everything is copied to a local
/// before the diverging resume, because nothing may be owned across a `-> !` call: a free emitted
/// there is sunk past the call and deleted as unreachable.
unsafe extern "C" fn comp_main_entry(args: usize) -> ! {
    let a = unsafe { core::ptr::read_unaligned(args as *const EntryArgs) };
    twizzler_abi::syscall::sys_sctx_attach(a.instance).unwrap();
    let flags = if a.suspend {
        ResumeFlags::SUSPEND
    } else {
        ResumeFlags::empty()
    };
    let frame = UpcallFrame::new_entry_frame(
        a.stack_ptr,
        a.stack_size,
        a.thread_ptr,
        a.instance,
        a.entry,
        a.arg,
    );
    unsafe { twizzler_abi::syscall::sys_thread_resume_from_upcall(&frame, flags) }
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
