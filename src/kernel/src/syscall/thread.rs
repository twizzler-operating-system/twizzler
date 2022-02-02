use alloc::sync::Arc;
use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadControl, ThreadSpawnArgs, ThreadSpawnError},
};

use crate::{obj::Object, thread::current_thread_ref};

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    let obj = Arc::new(Object::new());
    crate::obj::register_object(obj.clone());
    crate::thread::start_new_user(*args);
    Ok(obj.id())
}

pub fn thread_ctrl(cmd: ThreadControl, arg: u64) -> (u64, u64) {
    match cmd {
        ThreadControl::SetTls => {
            current_thread_ref().unwrap().set_tls(arg);
        }
        _ => todo!(),
    }
    (0, 0)
}
