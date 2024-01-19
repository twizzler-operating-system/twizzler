use std::{collections::HashMap, mem::MaybeUninit, sync::Mutex};

use twizzler_abi::{
    syscall::{sys_spawn, UpcallTargetSpawnOption},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_object::ObjID;
use twizzler_runtime_api::{SpawnError, ThreadSpawnArgs};

pub const SUPER_UPCALL_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB

pub fn spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, SpawnError> {
    tracing::info!("SPAWN THREAD IN MON");
    let super_stack = Box::<[u8]>::new_zeroed_slice(SUPER_UPCALL_STACK_SIZE);

    let upcall_target = UpcallTarget::new(
        None,
        Some(twz_rt::rr_upcall_entry),
        super_stack.as_ptr() as usize,
        SUPER_UPCALL_STACK_SIZE,
        0,
        0.into(),
        [UpcallOptions {
            flags: UpcallFlags::empty(),
            mode: UpcallMode::CallSuper,
        }; UpcallInfo::NR_UPCALLS],
    );

    let mut mgr = THREAD_MGR.lock().map_err(|_| SpawnError::Other)?;
    let thid = unsafe {
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
    }
    .map_err(|_| SpawnError::KernelError)?;

    mgr.all.insert(thid, ManagedThread::new(thid, super_stack));

    Ok(thid)
}

#[no_mangle]
pub fn __monitor_rt_spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, SpawnError> {
    spawn_thread(args, thread_pointer, stack_pointer)
}

#[allow(dead_code)]
struct ManagedThread {
    id: ObjID,
    super_stack: Box<[MaybeUninit<u8>]>,
}

impl ManagedThread {
    fn new(id: ObjID, super_stack: Box<[MaybeUninit<u8>]>) -> Self {
        Self { id, super_stack }
    }
}

#[derive(Default)]
struct ThreadManager {
    all: HashMap<ObjID, ManagedThread>,
}

lazy_static::lazy_static! {
static ref THREAD_MGR: Mutex<ThreadManager> = Mutex::new(ThreadManager::default());
}
