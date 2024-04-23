use std::{
    collections::HashMap,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

use dynlink::tls::TlsRegion;
use miette::IntoDiagnostic;
use twizzler_abi::{
    syscall::{
        sys_spawn, sys_thread_exit, sys_thread_resume_from_upcall, ThreadSyncSleep,
        UpcallTargetSpawnOption,
    },
    upcall::{UpcallFlags, UpcallFrame, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_runtime_api::{CoreRuntime, MapFlags, ObjID, SpawnError, ThreadSpawnArgs};
use twz_rt::RuntimeThreadControl;

use crate::{api::MONITOR_INSTANCE_ID, compman::COMPMAN};

pub const SUPER_UPCALL_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB
pub const DEFAULT_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB
pub const STACK_SIZE_MIN_ALIGN: usize = 0x1000; // 4K
pub const DEFAULT_TLS_ALIGN: usize = 0x1000;

use self::thread_cleaner::ThreadCleaner;

mod thread_cleaner;

#[allow(dead_code)]
pub struct ManagedThread {
    pub id: ObjID,
    super_stack: Box<[MaybeUninit<u8>]>,
    super_tls: TlsRegion,
}

impl Drop for ManagedThread {
    fn drop(&mut self) {
        tracing::trace!("dropping ManagedThread {}", self.id);
    }
}

pub type ManagedThreadRef = Arc<ManagedThread>;

impl ManagedThread {
    fn new(id: ObjID, super_stack: Box<[MaybeUninit<u8>]>) -> ManagedThreadRef {
        Arc::new(Self {
            id,
            super_stack,
            super_tls: todo!(),
        })
    }

    unsafe fn spawn_thread(
        start: usize,
        super_stack_start: usize,
        super_thread_pointer: usize,
        arg: usize,
    ) -> Result<ObjID, SpawnError> {
        let args = ThreadSpawnArgs {
            stack_size: SUPER_UPCALL_STACK_SIZE,
            start,
            arg,
        };

        do_spawn_thread(
            args,
            super_thread_pointer,
            super_stack_start,
            SUPER_UPCALL_STACK_SIZE,
            super_thread_pointer,
            super_stack_start,
            SUPER_UPCALL_STACK_SIZE,
        )
    }

    fn do_spawn(start: unsafe extern "C" fn(usize) -> !, arg: usize) -> Result<Self, SpawnError> {
        let mut cm = COMPMAN.lock();
        let mon_comp = cm.get_monitor_dynlink_compartment();
        let super_tls = mon_comp
            .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
                NonNull::new(std::alloc::alloc_zeroed(layout))
            })
            .map_err(|_| SpawnError::Other)?;
        drop(cm);
        let super_thread_pointer = super_tls.get_thread_pointer_value();

        let super_stack = Box::new_zeroed_slice(SUPER_UPCALL_STACK_SIZE);

        // Safety: we are allocating and tracking both the stack and the tls region for greater than the lifetime of
        // this thread. The start entry points to our given start function.
        let id = unsafe {
            Self::spawn_thread(
                start as *const () as usize,
                super_stack.as_ptr() as usize,
                super_thread_pointer,
                arg,
            )
        }?;

        Ok(Self {
            id,
            super_stack,
            super_tls,
        })
    }

    fn do_start(main: Box<dyn FnOnce()>) -> Result<Self, SpawnError> {
        let main_addr = Box::into_raw(Box::new(main)) as usize;
        unsafe extern "C" fn managed_thread_entry(main: usize) -> ! {
            {
                let main = Box::from_raw(main as *mut Box<dyn FnOnce()>);
                main();
            }

            sys_thread_exit(0);
        }

        Self::do_spawn(managed_thread_entry, main_addr)
    }

    fn waitable_until_exit(&self) -> ThreadSyncSleep {
        todo!()
    }

    fn has_exited(&self) -> bool {
        todo!()
    }
}

pub fn start_managed_thread(main: impl FnOnce() + 'static) -> Result<ManagedThreadRef, SpawnError> {
    let mt = Arc::new(ManagedThread::do_start(Box::new(main))?);
    THREAD_MGR.insert(mt.clone());
    Ok(mt)
}

#[derive(Default)]
struct ThreadManagerInner {
    all: HashMap<ObjID, ManagedThreadRef>,
    cleaner: Option<ThreadCleaner>,
}

impl ThreadManagerInner {
    fn get_cleaner_thread(&mut self) -> &ThreadCleaner {
        self.cleaner.get_or_insert(ThreadCleaner::new())
    }
}

pub struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

lazy_static::lazy_static! {
pub static ref THREAD_MGR: ThreadManager = ThreadManager { inner: Mutex::new(ThreadManagerInner::default())};
}

impl ThreadManager {
    pub fn insert(&self, th: ManagedThreadRef) {
        let mut inner = self.inner.lock().unwrap();
        inner.all.insert(th.id, th.clone());
        inner.get_cleaner_thread().track(th);
    }

    fn do_remove(&self, th: &ManagedThreadRef) {
        let mut inner = self.inner.lock().unwrap();
        inner.all.remove(&th.id);
    }

    pub fn remove(&self, th: &ManagedThreadRef) {
        let mut inner = self.inner.lock().unwrap();
        inner.get_cleaner_thread().untrack(th.id);
        inner.all.remove(&th.id);
    }

    pub fn get(&self, id: ObjID) -> Option<ManagedThreadRef> {
        self.inner.lock().unwrap().all.get(&id).cloned()
    }
}

unsafe fn do_spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
    stack_size: usize,
    super_thread_pointer: usize,
    super_stack_pointer: usize,
    super_stack_size: usize,
) -> Result<ObjID, SpawnError> {
    let upcall_target = UpcallTarget::new(
        None,
        Some(twz_rt::rr_upcall_entry),
        super_stack_pointer,
        super_stack_size,
        super_thread_pointer,
        MONITOR_INSTANCE_ID,
        [UpcallOptions {
            flags: UpcallFlags::empty(),
            mode: UpcallMode::CallSuper,
        }; UpcallInfo::NR_UPCALLS],
    );

    sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
        entry: args.start,
        stack_base: stack_pointer,
        stack_size: args.stack_size,
        tls: thread_pointer,
        arg: args.arg,
        flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
        vm_context_handle: None,
        upcall_target: UpcallTargetSpawnOption::SetTo(upcall_target),
    })
    .map_err(|_| SpawnError::KernelError)
}

pub unsafe fn jump_into_compartment(
    target: ObjID,
    stack_pointer: usize,
    thread_pointer: usize,
    entry: usize,
    arg: usize,
) -> ! {
    let frame = UpcallFrame::new_entry_frame(stack_pointer, thread_pointer, target, entry, arg);
    sys_thread_resume_from_upcall(&frame)
}
