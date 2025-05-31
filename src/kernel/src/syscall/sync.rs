use alloc::{collections::BTreeMap, vec::Vec};
use core::time::Duration;

use twizzler_abi::{
    syscall::{ThreadSync, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake},
    thread::ExecutionState,
};
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError},
    Result,
};

use crate::{
    memory::{
        context::{kernel_context, UserContext},
        VirtAddr,
    },
    obj::{LookupFlags, ObjectRef},
    once::Once,
    spinlock::Spinlock,
    thread::{current_memory_context, current_thread_ref, CriticalGuard, ThreadRef},
};

struct Requeue {
    list: Spinlock<BTreeMap<u64, ThreadRef>>,
}

/* TODO: make this thread-local */
static REQUEUE: Once<Requeue> = Once::new();

fn get_requeue_list() -> &'static Requeue {
    REQUEUE.call_once(|| Requeue {
        list: Spinlock::new(BTreeMap::new()),
    })
}

pub fn requeue_all() {
    let requeue = get_requeue_list();
    let mut list = requeue.list.lock();
    for (_, thread) in list.extract_if(|_, v| v.reset_sync_sleep_done()) {
        crate::sched::schedule_thread(thread);
    }
}

pub fn add_to_requeue(thread: ThreadRef) {
    let requeue = get_requeue_list();
    requeue.list.lock().insert(thread.id(), thread);
}

pub fn remove_from_requeue(thread: &ThreadRef) {
    let requeue = get_requeue_list();
    requeue.list.lock().remove(&thread.id());
}

// TODO: this is gross, we're manually trading out a critical guard with an interrupt guard because
// we don't want to get interrupted... we need a better way to do this kind of consumable "don't
// schedule until I say so".
pub fn finish_blocking(guard: CriticalGuard) {
    let thread = current_thread_ref().unwrap();
    crate::interrupt::with_disabled(|| {
        drop(guard);
        thread.set_state(ExecutionState::Sleeping);
        crate::sched::schedule(false);
        thread.set_state(ExecutionState::Running);
    });
}

// TODO: uses-virtaddr
fn get_obj_and_offset(addr: VirtAddr) -> Result<(ObjectRef, usize)> {
    // let t = current_thread_ref().unwrap();
    // TODO: prevent user from waiting on kernel object memory
    let user_vmc = current_memory_context();
    let vmc = user_vmc
        .as_ref()
        .map(|x| &**x)
        .unwrap_or_else(|| &kernel_context());
    let mapping = vmc
        .lookup_object(addr.try_into().map_err(|_| ArgumentError::InvalidAddress)?)
        .ok_or(ArgumentError::InvalidAddress)?;
    let offset = (addr.raw() as usize) % (1024 * 1024 * 1024); //TODO: arch-dep, centralize these calculations somewhere, see PageNumber
    Ok((mapping.object().clone(), offset))
}

fn get_obj(reference: ThreadSyncReference) -> Result<(ObjectRef, usize)> {
    Ok(match reference {
        ThreadSyncReference::ObjectRef(id, offset) => {
            let obj = match crate::obj::lookup_object(id, LookupFlags::empty()) {
                crate::obj::LookupResult::Found(o) => o,
                _ => return Err(ArgumentError::InvalidAddress.into()),
            };
            (obj, offset)
        }
        ThreadSyncReference::Virtual(addr) => {
            get_obj_and_offset(VirtAddr::new(addr as u64).unwrap())?
        }
        ThreadSyncReference::Virtual32(addr) => {
            get_obj_and_offset(VirtAddr::new(addr as u64).unwrap())?
        }
    })
}

struct SleepEvent {
    obj: ObjectRef,
    offset: usize,
    did_sleep: bool,
}

fn prep_sleep(sleep: &ThreadSyncSleep, first_sleep: bool) -> Result<SleepEvent> {
    let (obj, offset) = get_obj(sleep.reference)?;
    let did_sleep = if matches!(sleep.reference, ThreadSyncReference::Virtual32(_)) {
        obj.setup_sleep_word32(
            offset,
            sleep.op,
            sleep.value as u32,
            first_sleep,
            sleep.flags,
        )
    } else {
        obj.setup_sleep_word(offset, sleep.op, sleep.value, first_sleep, sleep.flags)
    };

    Ok(SleepEvent {
        obj,
        offset,
        did_sleep,
    })
}

fn undo_sleep(sleep: SleepEvent) {
    sleep.obj.remove_from_sleep_word(sleep.offset);
}

pub fn wakeup(wake: &ThreadSyncWake) -> Result<usize> {
    let (obj, offset) = get_obj(wake.reference)?;
    Ok(obj.wakeup_word(offset, wake.count))
}

fn thread_sync_cb_timeout(thread: ThreadRef) {
    if thread.reset_sync_sleep() {
        add_to_requeue(thread);
    }
    requeue_all();
}

fn simple_timed_sleep(timeout: &&mut Duration) {
    let thread = current_thread_ref().unwrap();
    thread.set_sync_sleep();
    requeue_all();
    let guard = thread.enter_critical();
    thread.set_sync_sleep_done();
    let timeout_key = crate::clock::register_timeout_callback(
        // TODO: fix all our time types
        timeout.as_nanos() as u64,
        thread_sync_cb_timeout,
        thread.clone(),
    );
    finish_blocking(guard);
    drop(timeout_key);
}

pub fn sys_thread_sync(ops: &mut [ThreadSync], timeout: Option<&mut Duration>) -> Result<usize> {
    //logln!("sleep: {:?}, {:?}", ops, timeout);
    if let Some(ref timeout) = timeout {
        if ops.is_empty() {
            simple_timed_sleep(timeout);
            return Ok(0);
        }
    }
    let mut ready_count = 0;
    let mut unsleeps = Vec::new();
    let mut num_sleepers = 0;

    for op in ops {
        match op {
            ThreadSync::Sleep(sleep, result) => match prep_sleep(sleep, unsleeps.is_empty()) {
                Ok(se) => {
                    num_sleepers += 1;
                    *result = Ok(if se.did_sleep { 0 } else { 1 });
                    if se.did_sleep {
                        unsleeps.push(se);
                    } else {
                        ready_count += 1;
                    }
                }
                Err(x) => *result = Err(x),
            },
            ThreadSync::Wake(wake, result) => match wakeup(wake) {
                Ok(count) => {
                    *result = Ok(count);
                    if count > 0 {
                        ready_count += 1;
                    }
                }
                Err(x) => {
                    *result = Err(x);
                }
            },
        }
    }
    let thread = current_thread_ref().unwrap();
    let should_sleep = unsleeps.len() == num_sleepers && num_sleepers > 0;
    let was_timedout = {
        requeue_all();
        let guard = thread.enter_critical();
        thread.set_sync_sleep_done();
        let timeout_key = if should_sleep {
            let timeout_key = timeout.map(|timeout| {
                crate::clock::register_timeout_callback(
                    // TODO: fix all our time types
                    timeout.as_nanos() as u64,
                    thread_sync_cb_timeout,
                    thread.clone(),
                )
            });
            timeout_key
        } else {
            None
        };
        if should_sleep {
            finish_blocking(guard);
        } else {
            drop(guard);
        }
        // If we have a timeout key, AND we don't find it during release, the timeout fired.
        timeout_key.map(|tk| !tk.release()).unwrap_or(false)
    };
    for op in unsleeps {
        undo_sleep(op);
    }
    thread.reset_sync_sleep_done();
    thread.reset_sync_sleep();
    if was_timedout && ready_count == 0 {
        Err(GenericError::TimedOut.into())
    } else {
        Ok(ready_count)
    }
}
