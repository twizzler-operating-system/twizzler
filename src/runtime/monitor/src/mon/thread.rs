use std::{collections::HashMap, mem::MaybeUninit, sync::Arc};

use dynlink::tls::TlsRegion;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{sys_spawn, sys_thread_exit, ThreadSyncSleep, UpcallTargetSpawnOption},
    thread::ThreadRepr,
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_runtime_api::{ObjID, SpawnError};

use super::space::MapHandle;
use crate::{api::MONITOR_INSTANCE_ID, thread::SUPER_UPCALL_STACK_SIZE};

mod cleaner;

pub struct ThreadMgr {
    all: HashMap<ObjID, ManagedThread>,
    cleaner: cleaner::ThreadCleaner,
}

impl ThreadMgr {
    fn do_remove(&mut self, thread: &ManagedThread) {
        todo!()
    }

    unsafe fn spawn_thread(
        start: usize,
        super_stack_start: usize,
        super_thread_pointer: usize,
        arg: usize,
    ) -> Result<ObjID, SpawnError> {
        let upcall_target = UpcallTarget::new(
            None,
            Some(twz_rt::rr_upcall_entry),
            super_stack_start,
            SUPER_UPCALL_STACK_SIZE,
            super_thread_pointer,
            MONITOR_INSTANCE_ID,
            [UpcallOptions {
                flags: UpcallFlags::empty(),
                mode: UpcallMode::CallSuper,
            }; UpcallInfo::NR_UPCALLS],
        );

        sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
            entry: start,
            stack_base: super_stack_start,
            stack_size: SUPER_UPCALL_STACK_SIZE,
            tls: super_thread_pointer,
            arg,
            flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
            vm_context_handle: None,
            upcall_target: UpcallTargetSpawnOption::SetTo(upcall_target),
        })
        .map_err(|_| SpawnError::KernelError)
    }

    fn do_spawn(start: unsafe extern "C" fn(usize) -> !, arg: usize) -> Result<Self, SpawnError> {
        /*
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

        // Safety: we are allocating and tracking both the stack and the tls region for greater than
        // the lifetime of this thread. The start entry points to our given start function.
        let id = unsafe {
            Self::spawn_thread(
                start as *const () as usize,
                super_stack.as_ptr() as usize,
                super_thread_pointer,
                arg,
            )
        }?;

        // TODO
        let repr = crate::mapman::map_object(MapInfo {
            id,
            flags: MapFlags::READ,
        })
        .unwrap();

        Ok(Self {
            id,
            super_stack,
            super_tls,
            repr: ManagedThreadRepr::new(repr),
        })
        */
        todo!()
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
}

#[allow(dead_code)]
pub struct ManagedThreadInner {
    pub id: ObjID,
    pub(crate) repr: ManagedThreadRepr,
    super_stack: Box<[MaybeUninit<u8>]>,
    super_tls: TlsRegion,
}

impl ManagedThreadInner {
    pub fn has_exited(&self) -> bool {
        todo!()
    }

    pub fn waitable_until_exit(&self) -> ThreadSyncSleep {
        todo!()
    }
}

// Safety: TlsRegion is not changed, and points to only globally- and permanently-allocated data.
unsafe impl Send for ManagedThreadInner {}
unsafe impl Sync for ManagedThreadInner {}

impl core::fmt::Debug for ManagedThreadInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ManagedThread({})", self.id)
    }
}

impl Drop for ManagedThreadInner {
    fn drop(&mut self) {
        tracing::trace!("dropping ManagedThread {}", self.id);
    }
}

pub type ManagedThread = Arc<ManagedThreadInner>;

pub(crate) struct ManagedThreadRepr {
    handle: MapHandle,
}

impl ManagedThreadRepr {
    fn new(handle: MapHandle) -> Self {
        Self { handle }
    }

    pub fn get_repr(&self) -> &ThreadRepr {
        let addr = self.handle.addrs().start + NULLPAGE_SIZE;
        unsafe { (addr as *const ThreadRepr).as_ref().unwrap() }
    }
}
