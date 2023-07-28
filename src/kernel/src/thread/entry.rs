use alloc::sync::Arc;
use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSpawnArgs, ThreadSpawnError},
};

use crate::{
    memory::VirtAddr,
    obj::ObjectRef,
    sched::schedule_new_thread,
    syscall::object::get_vmcontext_from_handle,
    userinit::{create_blank_object, user_init},
};

use super::{current_thread_ref, priority::Priority, Thread, ThreadRef};

extern "C" fn user_new_start() {
    let (entry, stack_base, stack_size, arg) = {
        /* we need this scope to drop the current thread ref before jumping to user */
        let current = current_thread_ref().unwrap();
        let args = current.spawn_args.as_ref().unwrap();
        current.set_tls(args.tls as u64);
        /*
        logln!(
            "thread jtu {:x} {:x} {:x}",
            args.entry,
            args.stack_base + args.stack_size,
            args.tls
        );
        */
        (args.entry, args.stack_base, args.stack_size, args.arg)
    };
    unsafe {
        crate::arch::jump_to_user(
            VirtAddr::new(entry as u64).unwrap(),
            /* TODO: this is x86 specific */
            VirtAddr::new((stack_base + stack_size - 8) as u64).unwrap(),
            arg as u64,
        )
    }
}

pub fn start_new_user(args: ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    let mut thread = if let Some(handle) = args.vm_context_handle {
        let vmc = get_vmcontext_from_handle(handle).ok_or(ThreadSpawnError::NotFound)?;
        Thread::new_with_handle_context(args, vmc)
    } else {
        Thread::new_with_current_context(args)
    };
    unsafe {
        thread.init(user_new_start);
    }
    thread.repr = Some(create_blank_object());
    let id = thread.repr.as_ref().unwrap().id();
    /*
    logln!(
        "starting new thread {} {} with stack k={:p} u={:x},{:x}",
        thread.id,
        id,
        thread.kernel_stack,
        args.stack_base,
        args.stack_size,
    );
    */
    schedule_new_thread(thread);
    Ok(id)
}

pub fn start_new_init() {
    let mut thread = Thread::new_with_new_vm();
    /*
    logln!(
        "starting new thread {} with stack {:p}",
        thread.id,
        thread.kernel_stack
    );
    */
    unsafe {
        thread.init(user_init);
    }
    thread.repr = Some(create_blank_object());
    schedule_new_thread(thread);
}

pub fn start_new_kernel(pri: Priority, start: extern "C" fn()) -> ThreadRef {
    let mut thread = Thread::new();
    thread.priority = pri;
    unsafe { thread.init(start) }
    schedule_new_thread(thread)
}
