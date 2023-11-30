use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadControl, ThreadSpawnArgs, ThreadSpawnError},
    upcall::UpcallTarget,
};

use crate::thread::current_thread_ref;

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    crate::thread::entry::start_new_user(*args)
}

pub fn thread_ctrl(cmd: ThreadControl, arg: u64) -> (u64, u64) {
    match cmd {
        ThreadControl::SetUpcall => {
            let Some(data) = (unsafe { (arg as usize as *const UpcallTarget).as_ref() }) else {
                return (1, 1);
            };
            // TODO: verify args, check perms.
            *current_thread_ref().unwrap().upcall_target.lock() = Some(*data);
        }
        ThreadControl::SetTls => {
            current_thread_ref().unwrap().set_tls(arg);
        }
        ThreadControl::Exit => {
            crate::thread::exit(arg);
        }
        ThreadControl::Yield => {
            // TODO: maybe give a priority drop?
            crate::sched::schedule(true);
        }
        _ => todo!(),
    }
    (0, 0)
}
