use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadControl, ThreadSpawnArgs, ThreadSpawnError},
};

use crate::{
    memory::context::UserContext,
    thread::{current_memory_context, current_thread_ref},
};

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    crate::thread::entry::start_new_user(*args)
}

pub fn thread_ctrl(cmd: ThreadControl, arg: u64) -> (u64, u64) {
    match cmd {
        ThreadControl::SetUpcall => {
            let ctx = current_memory_context().unwrap();
            if let Ok(arg) = arg.try_into() {
                ctx.set_upcall(arg);
            }
        }
        ThreadControl::SetTls => {
            current_thread_ref().unwrap().set_tls(arg);
        }
        ThreadControl::Exit => {
            {
                let th = current_thread_ref().unwrap();
                unsafe {
                    th.repr.as_ref().unwrap().write_val_and_signal(0x1000 /* TODO: object null page size */ + 16 /* TODO: thread repr status word */,
                    arg, usize::MAX);
                    th.repr.as_ref().unwrap().write_val_and_signal(0x1000 /* TODO: object null page size */ + 8 /* TODO: thread repr status word */,
                    1u64, usize::MAX);
                }
            }
            crate::thread::exit();
        }
        ThreadControl::Yield => {
            // TODO: maybe give a priority drop?
            crate::sched::schedule(true);
        } //_ => todo!(),
    }
    (0, 0)
}
