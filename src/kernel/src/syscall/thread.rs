use core::sync::atomic::Ordering;

use twizzler_abi::{
    arch::ArchRegisters,
    object::ObjID,
    syscall::{SctxSwitchFlags, ThreadControl, ThreadSchedStats, ThreadSpawnArgs},
    thread::ExecutionState,
    upcall::{ResumeFlags, UpcallFrame, UpcallTarget},
};
use twizzler_rt_abi::{error::TwzError, Result};

use crate::{
    processor::{
        mp::all_processors,
        sched::{lookup_thread_repr, schedule, SchedFlags},
    },
    security::SwitchResult,
    syscall::{
        object::sys_sctx_attach,
        sync::{add_to_requeue, requeue_all},
    },
    thread::{current_thread_ref, priority::Priority},
};

pub fn sys_spawn(args: &ThreadSpawnArgs) -> Result<ObjID> {
    crate::thread::entry::start_new_user(*args)
}

pub fn thread_ctrl(
    cmd: ThreadControl,
    target: Option<ObjID>,
    arg: u64,
    arg2: u64,
    arg3: u64,
) -> [u64; 2] {
    match cmd {
        ThreadControl::GetUpcall => {
            let arg = arg as usize as *mut UpcallTarget;
            // TODO: verify args, check perms.
            if let Some(target) = *current_thread_ref().unwrap().upcall_target.lock() {
                unsafe { arg.write(target) };
            } else {
                return [1, 1];
            }
        }
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
                log::debug!(
                    "resume-suspend: {:?} => {}",
                    current_thread_ref().unwrap().objid(),
                    data.prior_ctx,
                );
                current_thread_ref().unwrap().suspend();
            }
        }
        ThreadControl::SetTls => {
            current_thread_ref().unwrap().set_tls(arg);
        }
        ThreadControl::GetTls => {
            return [current_thread_ref().unwrap().get_tls(), 0];
        }
        ThreadControl::Exit => {
            crate::thread::exit(arg);
        }
        ThreadControl::Yield => {
            schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
        }
        ThreadControl::GetSelfId => return current_thread_ref().unwrap().objid().parts(),
        ThreadControl::GetActiveSctxId => {
            return current_thread_ref().unwrap().active_sctx_id().parts();
        }
        ThreadControl::SetActiveSctxId => {
            let id = ObjID::from_parts([arg, arg2]);
            let flags = SctxSwitchFlags::from_bits_truncate(arg3);
            let cur = current_thread_ref().unwrap();
            let (res, tls) = cur.switch_sctx(id);
            if res != SwitchResult::NotAttached {
                return [0, tls];
            }
            // A gate entry always switches into a context it is not necessarily attached to yet,
            // and attaching is unconditional anyway (any thread may attach any sctx it can name),
            // so doing it here saves the caller a second syscall on every cold entry -- and, since
            // userspace cannot cheaply remember that it has already attached, on every entry.
            if !flags.contains(SctxSwitchFlags::ATTACH) {
                return [1, 1];
            }
            if let Err(e) = sys_sctx_attach(id) {
                return [1, e.raw()];
            }
            let (res, tls) = cur.switch_sctx(id);
            return match res {
                SwitchResult::NotAttached => [1, 1],
                _ => [0, tls],
            };
        }
        ThreadControl::GetStats => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let stats_ptr = arg as usize as *mut ThreadSchedStats;
            let stats_ptr = unsafe { stats_ptr.as_mut() }.ok_or(TwzError::INVALID_ARGUMENT);
            if let Ok(stats_ptr) = stats_ptr {
                stats_ptr.idle = thread.stats.idle.load(Ordering::SeqCst);
                stats_ptr.system = thread.stats.sys.load(Ordering::SeqCst);
                stats_ptr.user = thread.stats.user.load(Ordering::SeqCst);
            } else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            }

            return [0, 0];
        }
        ThreadControl::ReadRegisters => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
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
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let target_state = ExecutionState::from_status(arg);
            let cur_state = thread.get_state();
            log::debug!(
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
                    ExecutionState::Exited => {
                        // `exit()` unlinks the target from the scheduler and the requeue list but
                        // never from a mutex wait queue, and this is the only call site that can
                        // kill a thread mid-wait. `mutex_link` linked here says the target dies
                        // while still a member of some mutex's sleep queue.
                        let exit_sctx = ObjID::from_parts([arg2, arg3]);
                        log::debug!(
                            "force exit on thread {} ({}), state {:?}, mutex_linked {}, mutex_wait {}, active sctx {}, exit sctx {}",
                            thread.id(),
                            thread.objid(),
                            cur_state,
                            thread.mutex_link.is_linked(),
                            thread.get_mutex_wait(),
                            thread.active_sctx_id(),
                            exit_sctx,
                        );
                        thread.set_exit_sctx(exit_sctx);
                        thread.force_exit();
                    }
                    _ => {
                        return [1, TwzError::INVALID_ARGUMENT.raw()];
                    }
                }
            }

            return [0, cur_state.to_status()];
        }
        ThreadControl::GetTraceEvents => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let events = thread.get_trace_state();
            return match events {
                Ok(events) => [0, events],
                Err(e) => [1, e.raw()],
            };
        }
        ThreadControl::SetTraceEvents => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let events = thread.set_trace_state(arg);
            return match events {
                Ok(_) => [0, 0],
                Err(e) => [1, e.raw()],
            };
        }
        ThreadControl::SetPriority => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let Ok(raw) = u32::try_from(arg) else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            let Some(pri) = Priority::try_from_raw(raw) else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            // TODO: check perms (raising priority, and touching another thread, should both be
            // privileged).
            thread.set_priority(pri);
            return [0, 0];
        }
        ThreadControl::GetPriority => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            return [0, thread.base_priority().raw() as u64];
        }
        ThreadControl::SendMessage => {
            let thread = if let Some(target) = target {
                lookup_thread_repr(target)
            } else {
                current_thread_ref().cloned()
            };
            let Some(thread) = thread else {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            };
            // The mailbox is a bitmask, not a slot: a plain store let a second message
            // overwrite a first that had not been delivered yet, and the window is wide
            // because delivery waits for the thread to return to user in its own security
            // context. A message therefore names a bit, and zero means "nothing pending".
            if arg == 0 || arg >= u64::BITS as u64 {
                return [1, TwzError::INVALID_ARGUMENT.raw()];
            }
            thread.pending_message.fetch_or(1 << arg, Ordering::SeqCst);
            if thread.reset_sync_sleep() {
                add_to_requeue(thread);
            }
            requeue_all();
            for p in all_processors().iter() {
                if let Some(p) = p {
                    if p.is_running() {
                        p.wakeup(true);
                    }
                }
            }
        }
        _ => {
            return [1, 1];
        }
    }
    [0, 0]
}
