use alloc::vec::Vec;
use core::time::Duration;

use intrusive_collections::{intrusive_adapter, KeyAdapter, RBTree};
use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSync, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake, TimeSpan},
    thread::ExecutionState,
    trace::{ThreadBlocked, ThreadResumed, TraceEntryFlags, TraceKind, MAX_BLOCK_NAME},
};
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError},
    Result,
};

use crate::{
    instant::Instant,
    memory::{
        context::{kernel_context, UserContext},
        VirtAddr,
    },
    obj::{LookupFlags, ObjectRef},
    once::Once,
    spinlock::Spinlock,
    thread::{current_memory_context, current_thread_ref, CriticalGuard, Thread, ThreadRef},
    trace::{
        mgr::{TraceEvent, TRACE_MGR},
        new_trace_entry,
    },
};

struct Requeue {
    //list: Spinlock<BTreeMap<u64, ThreadRef>>,
    list: Spinlock<RBTree<RequeueLinkAdapter>>,
}

intrusive_adapter!(pub RequeueLinkAdapter = ThreadRef: Thread { requeue_link: intrusive_collections::rbtree::AtomicLink });

impl<'a> KeyAdapter<'a> for RequeueLinkAdapter {
    type Key = ObjID;
    fn get_key(&self, s: &'a Thread) -> ObjID {
        s.objid()
    }
}

/* TODO: make this thread-local */
static REQUEUE: Once<Requeue> = Once::new();

fn get_requeue_list() -> &'static Requeue {
    REQUEUE.call_once(|| Requeue {
        list: Spinlock::new(RBTree::new(RequeueLinkAdapter::NEW)),
    })
}

pub fn requeue_all() {
    let requeue = get_requeue_list();
    let mut list = requeue.list.lock();
    let mut cursor = list.cursor_mut();
    cursor.move_next();
    while !cursor.is_null() {
        if cursor
            .get()
            .is_some_and(|v| !v.is_critical() && v.reset_sync_sleep_done())
        {
            if let Some(t) = cursor.remove() {
                crate::sched::schedule_thread(t);
            }
        } else {
            cursor.move_next();
        }
    }
}

pub fn add_to_requeue(thread: ThreadRef) {
    let requeue = get_requeue_list();
    requeue.list.lock().insert(thread);
}

pub fn remove_from_requeue(thread: &ThreadRef) {
    let requeue = get_requeue_list();
    let mut list = requeue.list.lock();
    let _ = list.find_mut(&thread.objid()).remove();
}

pub fn trace_block(_th: &ThreadRef, name: impl AsRef<str>) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_BLOCK) {
        let name = name.as_ref();
        let mut block_name = [0; MAX_BLOCK_NAME];
        let len = name.as_bytes().len().min(MAX_BLOCK_NAME);
        (&mut block_name[0..len]).copy_from_slice(&name.as_bytes()[0..len]);
        let block = ThreadBlocked {
            block_name,
            block_name_len: len as u32,
        };
        let entry = new_trace_entry(
            TraceKind::Thread,
            twizzler_abi::trace::THREAD_BLOCK,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, block));
    }
}

pub fn trace_resume(_th: &ThreadRef, duration: TimeSpan) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_RESUME) {
        let data = ThreadResumed { duration };
        let entry = new_trace_entry(
            TraceKind::Thread,
            twizzler_abi::trace::THREAD_RESUME,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, data));
    }
}
// TODO: this is gross, we're manually trading out a critical guard with an interrupt guard because
// we don't want to get interrupted... we need a better way to do this kind of consumable "don't
// schedule until I say so".
pub fn finish_blocking(guard: CriticalGuard) {
    let thread = current_thread_ref().unwrap();
    let start = Instant::now();
    trace_block(&thread, "thread-sync");
    crate::interrupt::with_disabled(|| {
        drop(guard);
        thread.set_state(ExecutionState::Sleeping);
        crate::sched::schedule(false);
        thread.set_state(ExecutionState::Running);
    });
    let end = Instant::now();
    trace_resume(&thread, (end - start).into());
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

fn undo_sleep(sleep: &SleepEvent) {
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
    let timeout_key = crate::clock::register_timeout_callback(
        // TODO: fix all our time types
        timeout.as_nanos() as u64,
        thread_sync_cb_timeout,
        thread.clone(),
    );
    requeue_all();
    let guard = thread.enter_critical();
    thread.set_sync_sleep_done();
    finish_blocking(guard);
    remove_from_requeue(&thread);
    drop(timeout_key);
}

pub fn sys_thread_sync(ops: &mut [ThreadSync], timeout: Option<&mut Duration>) -> Result<usize> {
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
    let timeout_key = {
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
        requeue_all();
        let guard = thread.enter_critical();
        thread.set_sync_sleep_done();
        if should_sleep {
            finish_blocking(guard);
        } else {
            if thread.reset_sync_sleep() {
                add_to_requeue(thread.clone());
            }
            requeue_all();
            if unsleeps.len() > 0 {
                finish_blocking(guard);
            } else {
                drop(guard);
            }
        }
        timeout_key
    };
    for op in &unsleeps {
        undo_sleep(op);
    }
    thread.reset_sync_sleep_done();
    thread.reset_sync_sleep();
    remove_from_requeue(&thread);
    drop(unsleeps);
    // If we have a timeout key, AND we don't find it during release, the timeout fired.
    let was_timedout = timeout_key.map(|tk| !tk.release()).unwrap_or(false);
    if was_timedout && ready_count == 0 {
        Err(GenericError::TimedOut.into())
    } else {
        Ok(ready_count)
    }
}
