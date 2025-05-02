use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadControl, ThreadSpawnArgs},
    upcall::{UpcallFrame, UpcallTarget},
};
use twizzler_rt_abi::Result;

use crate::{security::SwitchResult, thread::current_thread_ref};

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID> {
    crate::thread::entry::start_new_user(*args)
}

pub fn thread_ctrl(cmd: ThreadControl, arg: u64, arg2: u64) -> [u64; 2] {
    match cmd {
        ThreadControl::SetUpcall => {
            let Some(data) = (unsafe { (arg as usize as *const UpcallTarget).as_ref() }) else {
                return [1, 1];
            };
            // TODO: verify args, check perms.
            *current_thread_ref().unwrap().upcall_target.lock() = Some(*data);
        }
        ThreadControl::ResumeFromUpcall => {
            let Some(data) = (unsafe { (arg as usize as *const UpcallFrame).as_ref() }) else {
                return [1, 1];
            };
            // TODO: verify args, check perms.

            current_thread_ref().unwrap().restore_upcall_frame(data);
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
        ThreadControl::GetSelfId => return current_thread_ref().unwrap().objid().parts(),
        ThreadControl::GetActiveSctxId => {
            return current_thread_ref().unwrap().secctx.active_id().parts()
        }
        ThreadControl::SetActiveSctxId => {
            let id = ObjID::from_parts([arg, arg2]);
            return match current_thread_ref().unwrap().secctx.switch_context(id) {
                SwitchResult::NotAttached => [1, 1],
                _ => [0, 0],
            };
        }
        _ => todo!(),
    }
    [0, 0]
}
