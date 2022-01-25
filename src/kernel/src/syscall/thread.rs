use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSpawnArgs, ThreadSpawnError, ThreadControl},
};

use crate::thread::current_thread_ref;

pub fn sys_spawn(_args: &ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    todo!()
}

pub fn thread_ctrl(cmd: ThreadControl, arg: u64) -> (u64, u64) {
    match cmd {
        ThreadControl::SetTls => {
            current_thread_ref().unwrap().set_tls(arg);
        },
        _ => todo!()
    }
    (0, 0)
}