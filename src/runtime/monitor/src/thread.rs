use std::{collections::HashMap, mem::MaybeUninit, ptr::NonNull, sync::Mutex};

use monitor_api::SharedCompConfig;
use twizzler_abi::{
    syscall::{sys_spawn, UpcallTargetSpawnOption},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_object::ObjID;
use twizzler_runtime_api::{SpawnError, ThreadSpawnArgs};
use twz_rt::monitor::RuntimeThreadControl;

use crate::state::get_monitor_state;

pub const SUPER_UPCALL_STACK_SIZE: usize = 8 * 1024 * 1024; // 8MB

pub fn spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, SpawnError> {
    // Allocate a new stack for super entry for upcalls.
    let super_stack = Box::<[u8]>::new_zeroed_slice(SUPER_UPCALL_STACK_SIZE);

    let mut state = get_monitor_state().lock().unwrap();
    let mon_comp = state.get_monitor_compartment_mut();
    let tls = mon_comp
        .build_tls_region(RuntimeThreadControl::default(), |layout| unsafe {
            NonNull::new(std::alloc::alloc_zeroed(layout))
        })
        .unwrap();

    let upcall_target = UpcallTarget::new(
        None,
        Some(twz_rt::rr_upcall_entry),
        super_stack.as_ptr() as usize,
        SUPER_UPCALL_STACK_SIZE,
        tls.get_thread_pointer_value(),
        0.into(),
        [UpcallOptions {
            flags: UpcallFlags::empty(),
            mode: UpcallMode::CallSuper,
        }; UpcallInfo::NR_UPCALLS],
    );

    // Lock before spawn so we guarantee we can fill out the manager entry before the thread can look there.
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

// Extern function, linked to by the runtime.
#[no_mangle]
pub fn __monitor_rt_spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, SpawnError> {
    spawn_thread(args, thread_pointer, stack_pointer)
}

// Extern function, linked to by the runtime.
#[no_mangle]
pub fn __monitor_rt_get_comp_config(_comp: ObjID) -> *const SharedCompConfig {
    let state = get_monitor_state().lock().unwrap();
    let comp = state.comps.get(&0.into()).unwrap();
    comp.get_comp_config()
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
