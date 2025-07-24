use twizzler_abi::{
    arch::ArchRegisters,
    object::ObjID,
    syscall::{ThreadControl, ThreadSpawnArgs},
    thread::ExecutionState,
    upcall::{ResumeFlags, UpcallFrame, UpcallTarget},
};
use twizzler_rt_abi::{error::TwzError, Result};

use crate::{security::SwitchResult, thread::current_thread_ref};

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID> {
    crate::thread::entry::start_new_user(*args)
}

pub fn thread_ctrl(cmd: ThreadControl, target: Option<ObjID>, arg: u64, arg2: u64) -> [u64; 2] {
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
            let flags = ResumeFlags::from_bits_truncate(arg2);
            // TODO: verify args, check perms.

            current_thread_ref().unwrap().restore_upcall_frame(data);

            if flags.contains(ResumeFlags::SUSPEND) {
                log::info!(
                    "resume-suspend: {:?}",
                    current_thread_ref().unwrap().objid()
                );
                current_thread_ref().unwrap().suspend();
            }
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
        ThreadControl::ReadRegisters => {
            let thread = if let Some(target) = target {
                crate::sched::lookup_thread_repr(target)
            } else {
                current_thread_ref()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let ptr = arg as usize as *mut ArchRegisters;
            let regs = match thread.read_registers() {
                Ok(regs) => regs,
                Err(e) => return [1, e.raw()],
            };
            unsafe { ptr.write(regs) };
        }
        ThreadControl::ChangeState => {
            let thread = if let Some(target) = target {
                crate::sched::lookup_thread_repr(target)
            } else {
                current_thread_ref()
            };
            let Some(thread) = thread else {
                log::info!("could not find {:?}", target);
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let target_state = ExecutionState::from_status(arg);
            let cur_state = thread.get_state();
            log::info!(
                "change state {:?}: {:?} => {:?}",
                target,
                cur_state,
                target_state
            );
            if cur_state == ExecutionState::Exited {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            }
            if cur_state != target_state {
                match target_state {
                    ExecutionState::Running => {
                        thread.unsuspend_thread();
                    }
                    ExecutionState::Suspended => {
                        thread.suspend();
                    }
                    _ => {
                        return [1, TwzError::INVALID_ARGUMENT.raw()];
                    }
                }
            }

            return [0, cur_state.to_status()];
        }
        _ => {
            return [1, 1];
        }
    }
    [0, 0]
}
