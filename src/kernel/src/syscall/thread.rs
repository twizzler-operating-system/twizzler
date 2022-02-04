use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadControl, ThreadSpawnArgs, ThreadSpawnError},
};

use crate::thread::current_thread_ref;

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    let id = crate::thread::start_new_user(*args);
    Ok(id)
}

pub fn thread_ctrl(cmd: ThreadControl, arg: u64) -> (u64, u64) {
    match cmd {
        ThreadControl::SetTls => {
            current_thread_ref().unwrap().set_tls(arg);
        }
        ThreadControl::Exit => {
            {
                let th = current_thread_ref().unwrap();
                unsafe {
                    th.repr.as_ref().unwrap().write_val_and_signal(0x1000 /* TODO: object null page size */ + 8 /* TODO: thread repr status word */,
                    1u64, usize::MAX);
                }
            }
            crate::thread::exit();
        }
        _ => todo!(),
    }
    (0, 0)
}
