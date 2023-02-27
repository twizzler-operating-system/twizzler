use core::time::Duration;

use alloc::{collections::BTreeMap, vec::Vec};
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncError, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};

use crate::{
    memory::{context::UserContext, VirtAddr},
    obj::{LookupFlags, ObjectRef},
    once::Once,
    spinlock::Spinlock,
    thread::{current_memory_context, current_thread_ref, CriticalGuard, ThreadRef, ThreadState},
};

struct Requeue {
    list: Spinlock<BTreeMap<u64, ThreadRef>>,
}

/* TODO: make this thread-local */
static mut REQUEUE: Once<Requeue> = Once::new();

fn get_requeue_list() -> &'static Requeue {
    unsafe {
        REQUEUE.call_once(|| Requeue {
            list: Spinlock::new(BTreeMap::new()),
        })
    }
}

pub fn requeue_all() {
    let requeue = get_requeue_list();
    let mut list = requeue.list.lock();
    for (_, thread) in list.drain_filter(|_, v| v.reset_sync_sleep_done()) {
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
fn finish_blocking(guard: CriticalGuard) {
    let thread = current_thread_ref().unwrap();
    crate::interrupt::with_disabled(|| {
        thread.set_state(ThreadState::Blocked);
        drop(guard);
        crate::sched::schedule(false);
        thread.set_state(ThreadState::Running);
    });
}

// TODO: uses-virtaddr
fn get_obj_and_offset(addr: VirtAddr) -> Result<(ObjectRef, usize), ThreadSyncError> {
    // let t = current_thread_ref().unwrap();
    let vmc = current_memory_context().ok_or(ThreadSyncError::Unknown)?;
    let mapping = vmc
        .lookup_object(
            addr.try_into()
                .map_err(|_| ThreadSyncError::InvalidReference)?,
        )
        .ok_or(ThreadSyncError::InvalidReference)?;
    let offset = (addr.raw() as usize) % (1024 * 1024 * 1024); //TODO: arch-dep, centralize these calculations somewhere, see PageNumber
    Ok((mapping.object().clone(), offset))
}

fn get_obj(reference: ThreadSyncReference) -> Result<(ObjectRef, usize), ThreadSyncError> {
    Ok(match reference {
        ThreadSyncReference::ObjectRef(id, offset) => {
            let obj = match crate::obj::lookup_object(id, LookupFlags::empty()) {
                crate::obj::LookupResult::Found(o) => o,
                _ => return Err(ThreadSyncError::InvalidReference),
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

fn prep_sleep(sleep: &ThreadSyncSleep, first_sleep: bool) -> Result<SleepEvent, ThreadSyncError> {
    let (obj, offset) = get_obj(sleep.reference)?;
    /*
    logln!(
        "{} sleep {} {:x}",
        current_thread_ref().unwrap().id(),
        obj.id(),
        offset
    );
    if let ThreadSyncReference::Virtual(p) = &sleep.reference {
        logln!("  => {:p} {}", *p, unsafe {
            (**p).load(core::sync::atomic::Ordering::SeqCst)
        });
    }
    */
    let did_sleep = obj.setup_sleep_word(offset, sleep.op, sleep.value, first_sleep);
    Ok(SleepEvent {
        obj,
        offset,
        did_sleep,
    })
}

fn undo_sleep(sleep: SleepEvent) {
    sleep.obj.remove_from_sleep_word(sleep.offset);
}

fn wakeup(wake: &ThreadSyncWake) -> Result<usize, ThreadSyncError> {
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
    let guard = thread.enter_critical();
    thread.set_sync_sleep();
    crate::clock::register_timeout_callback(
        // TODO: fix all our time types
        timeout.as_nanos() as u64,
        thread_sync_cb_timeout,
        thread.clone(),
    );
    thread.set_sync_sleep_done();
    requeue_all();
    finish_blocking(guard);
}

// TODO: #42 on timeout, try to return Err(Timeout).
pub fn sys_thread_sync(
    ops: &mut [ThreadSync],
    timeout: Option<&mut Duration>,
) -> Result<usize, ThreadSyncError> {
    if let Some(ref timeout) = timeout {
        if ops.is_empty() {
            simple_timed_sleep(timeout);
            return Ok(0);
        }
    }
    let mut ready_count = 0;
    let mut unsleeps = Vec::new();

    for op in ops {
        match op {
            ThreadSync::Sleep(sleep, result) => match prep_sleep(sleep, unsleeps.is_empty()) {
                Ok(se) => {
                    *result = Ok(if se.did_sleep { 0 } else { 1 });
                    if se.did_sleep {
                        unsleeps.push(se);
                    } else {
                        ready_count += 1;
                    }
                }
                Err(x) => *result = Err(x),
            },
            ThreadSync::Wake(wake, result) => {
                /*
                if let ThreadSyncReference::Virtual(p) = &wake.reference {
                    logln!(" wake => {:p} {}", *p, unsafe {
                        (**p).load(core::sync::atomic::Ordering::SeqCst)
                    });
                }
                */
                match wakeup(wake) {
                    Ok(count) => {
                        *result = Ok(count);
                        if count > 0 {
                            ready_count += 1;
                        }
                    }
                    Err(x) => {
                        *result = Err(x);
                    }
                }
            }
        }
    }
    let thread = current_thread_ref().unwrap();
    {
        let guard = thread.enter_critical();
        if !unsleeps.is_empty() {
            if let Some(timeout) = timeout {
                crate::clock::register_timeout_callback(
                    // TODO: fix all our time types
                    timeout.as_nanos() as u64,
                    thread_sync_cb_timeout,
                    thread.clone(),
                );
            }
            thread.set_sync_sleep_done();
        }
        requeue_all();
        if !unsleeps.is_empty() {
            finish_blocking(guard);
        } else {
            drop(guard);
        }
    }
    for op in unsleeps {
        undo_sleep(op);
    }
    Ok(ready_count)
}
