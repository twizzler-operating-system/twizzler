use alloc::{boxed::Box, sync::Arc};
use core::mem::MaybeUninit;

use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSpawnArgs, ThreadSpawnFlags, UpcallTargetSpawnOption},
};
use twizzler_rt_abi::error::ArgumentError;

use super::{current_memory_context, current_thread_ref, priority::Priority, Thread, ThreadRef};
use crate::{
    condvar::CondVar,
    memory::{context::Context, VirtAddr},
    processor::current_processor,
    sched::schedule_new_thread,
    security::{SecCtxMgr, SecurityContext},
    spinlock::Spinlock,
    syscall::object::get_vmcontext_from_handle,
    userinit::user_init,
};

extern "C" fn user_new_start() {
    let (entry, stack_base, stack_size, arg) = {
        /* we need this scope to drop the current thread ref before jumping to user */
        let current = current_thread_ref().unwrap();
        let args = current.spawn_args.as_ref().unwrap();
        current.set_tls(args.tls as u64);
        (args.entry, args.stack_base, args.stack_size, args.arg)
    };
    unsafe {
        crate::arch::jump_to_user(
            VirtAddr::new(entry as u64).unwrap(),
            crate::arch::thread::new_stack_top(stack_base, stack_size),
            arg as u64,
        )
    }
}

pub fn start_new_user(args: ThreadSpawnArgs) -> twizzler_rt_abi::Result<ObjID> {
    let mut thread = if let Some(handle) = args.vm_context_handle {
        let vmc = get_vmcontext_from_handle(handle).ok_or(ArgumentError::BadHandle)?;
        Thread::new(Some(vmc), Some(args), Priority::default_user())
    } else {
        Thread::new(
            current_memory_context(),
            Some(args),
            Priority::default_user(),
        )
    };
    match args.upcall_target {
        UpcallTargetSpawnOption::DefaultAbort => {}
        UpcallTargetSpawnOption::Inherit => {
            *thread.upcall_target.lock() =
                current_thread_ref().and_then(|cth| *cth.upcall_target.lock());
        }
        UpcallTargetSpawnOption::SetTo(ut) => *thread.upcall_target.lock() = Some(ut),
    }
    if let Some(cur) = current_thread_ref() {
        thread.secctx = cur.secctx.clone();
    }
    unsafe {
        thread.init(user_new_start);
    }
    let id = thread.control_object.object().id();
    schedule_new_thread(thread);
    Ok(id)
}

pub fn start_new_init() {
    let mut thread = Thread::new(
        Some(Arc::new(Context::new())),
        None,
        Priority::default_user(),
    );
    thread.secctx = SecCtxMgr::new(Arc::new(SecurityContext::new(None)));
    unsafe {
        thread.init(user_init);
    }
    schedule_new_thread(thread);
}

pub fn start_new_kernel(pri: Priority, start: extern "C" fn(), arg: usize) -> ThreadRef {
    let mut thread = Thread::new(None, None, pri);
    unsafe { thread.init(start) }
    thread.spawn_args = Some(ThreadSpawnArgs {
        entry: 0,
        stack_base: 0,
        stack_size: 0,
        tls: 0,
        arg,
        flags: ThreadSpawnFlags::empty(),
        vm_context_handle: None,
        upcall_target: UpcallTargetSpawnOption::DefaultAbort,
    });
    schedule_new_thread(thread)
}

/// Handle for running a closure in another thread.
pub struct KthreadClosure<F, R> {
    closure: Spinlock<Box<Option<F>>>,
    // TODO: make this a mutex
    result: Spinlock<(bool, MaybeUninit<R>)>,
    signal: CondVar,
}

impl<F, R> KthreadClosure<F, R> {
    /// Wait for the other thread to finish and provide the result.
    #[track_caller]
    pub fn wait(self: Arc<Self>) -> R {
        loop {
            current_processor().cleanup_exited();
            let guard = self.result.lock();
            if guard.0 {
                // Safety: we only assume init if the flag is true, which is only set to true once
                // we initialize the MaybeUninit.
                return unsafe { guard.1.assume_init_read() };
            }
            self.signal.wait(guard);
        }
    }
}

struct KthreadArg {
    main: Box<dyn FnOnce(usize)>,
    arg: usize,
}

/// Run a closure on a new thread. Returns both the handle to the thread and also a handle that
/// allows the caller to wait for the result.
pub fn run_closure_in_new_thread<F, R>(
    pri: Priority,
    f: F,
) -> (ThreadRef, Arc<KthreadClosure<F, R>>)
where
    F: (FnOnce() -> R) + Send,
{
    let main = move |arg: usize| {
        // Safety: this pointer is generated below by a call to Arc::into_raw, and is guaranteed to
        // have a valid count by the code that generates this pointer.
        let info = unsafe { Arc::from_raw(arg as *const KthreadClosure<F, R>) };
        // Take this out, but don't hold the lock when we run the closure.
        let closure = { info.closure.lock().take().unwrap() };
        let result = (closure)();
        let mut guard = info.result.lock();
        guard.1.write(result);
        guard.0 = true;
        info.signal.signal();
    };

    extern "C" fn trampoline() {
        {
            let arg = current_thread_ref().unwrap().spawn_args.unwrap().arg;
            // Safety: this pointer is generated by a call to Box::into_raw, below.
            let arg = unsafe { Box::from_raw(arg as *mut KthreadArg) };
            (arg.main)(arg.arg);
        }
        crate::thread::exit(0);
    }

    let info = Arc::new(KthreadClosure {
        closure: Spinlock::new(Box::new(Some(f))),
        result: Spinlock::new((false, MaybeUninit::uninit())),
        signal: CondVar::new(),
    });
    let raw = Arc::into_raw(info);
    // Safety: manually increment the strong count so we can pass the raw pointer to the new thread.
    // That thread will call Arc::from_raw, gaining a valid Arc.
    unsafe {
        Arc::increment_strong_count(raw);
    }
    let arg = Box::new(KthreadArg {
        main: Box::new(main),
        arg: raw as usize,
    });
    let thr = start_new_kernel(pri, trampoline, Box::into_raw(arg) as usize);
    // Safety: this is our own Arc, from earlier, after we manually incremented the count on behalf
    // of the receiving thread.
    let info = unsafe { Arc::from_raw(raw) };
    (thr, info)
}

#[cfg(test)]
mod test {
    use twizzler_kernel_macros::kernel_test;

    use crate::thread::Priority;

    #[kernel_test]
    fn test_closure() {
        let x = super::run_closure_in_new_thread(Priority::default_user(), || 42)
            .1
            .wait();
        assert_eq!(42, x);
    }
}
