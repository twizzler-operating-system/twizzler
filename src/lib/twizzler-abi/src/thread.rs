use core::{
    alloc::Layout,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

#[allow(unused_imports)]
use crate::{
    object::{ObjID, Protections},
    simple_idcounter::IdCounter,
    syscall::{MapFlags, ThreadSpawnArgs, ThreadSpawnFlags},
};

#[repr(C)]
pub struct ThreadRepr {
    version: u32,
    flags: u32,
    status: AtomicU64,
}

#[allow(dead_code)]
struct Thread {
    objid: ObjID,
    ptr: *mut ThreadRepr,
    slot: usize,
    tls_base: *const u8,
    tls_len: usize,
    tls_align: usize,
    stack_base: *const u8,
    stack_len: usize,
    internal_id: u32,
}

impl Thread {
    fn get_repr(&self) -> &ThreadRepr {
        unsafe { self.ptr.as_ref().unwrap() }
    }
}

static THREADS_LOCK: crate::simple_mutex::Mutex = crate::simple_mutex::Mutex::new();
static mut THREADS: *mut Thread = ptr::null_mut();
static mut THREADS_LEN: usize = 0;
static mut THREAD_IDS: IdCounter = IdCounter::new(1);

const STACK_ALIGN: usize = 32;

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
unsafe fn new_thread(
    objid: ObjID,
    base: *mut ThreadRepr,
    tls_base: *const u8,
    tls_len: usize,
    tls_align: usize,
    stack_base: *const u8,
    stack_len: usize,
    slot: usize,
) -> u32 {
    assert!(THREADS_LOCK.is_locked());
    let id = THREAD_IDS.next();

    if id as usize >= THREADS_LEN {
        let new_len = core::cmp::max(THREADS_LEN * 2, 16);
        let new_size = new_len * core::mem::size_of::<Thread>();
        let old_size = THREADS_LEN * core::mem::size_of::<Thread>();
        let layout = Layout::from_size_align(old_size, core::mem::align_of::<Thread>()).unwrap();
        THREADS = crate::alloc::global_realloc(THREADS as *mut u8, layout, new_size) as *mut Thread;
        THREADS_LEN = new_len;
    }

    let slice = core::slice::from_raw_parts_mut(THREADS, THREADS_LEN);
    slice[id as usize] = Thread {
        objid,
        ptr: base,
        internal_id: id,
        tls_base,
        tls_len,
        tls_align,
        stack_base,
        stack_len,
        slot,
    };

    id
}

unsafe fn release_thread(id: u32) {
    assert!(THREADS_LOCK.is_locked());
    let (stack_base, stack_len, tls_base, tls_len, tls_align) = {
        let slice = core::slice::from_raw_parts_mut(THREADS, THREADS_LEN);
        let info = (
            slice[id as usize].stack_base,
            slice[id as usize].stack_len,
            slice[id as usize].tls_base,
            slice[id as usize].tls_len,
            slice[id as usize].tls_align,
        );
        if slice[id as usize].ptr.is_null() {
            // already released
            return;
        }
        slice[id as usize].ptr = ptr::null_mut();
        slice[id as usize].objid = ObjID::new(0);
        slice[id as usize].internal_id = 0;
        THREAD_IDS.release(id);
        info
    };
    let tls_layout = Layout::from_size_align(tls_len, tls_align).unwrap();
    let stack_layout = Layout::from_size_align(stack_len, STACK_ALIGN).unwrap();
    crate::alloc::global_free(tls_base as *mut u8, tls_layout);
    crate::alloc::global_free(stack_base as *mut u8, stack_layout);
}

#[cfg(any(doc, feature = "rt"))]
/// Spawn a new thread, allocating a new stack for it, starting it at the specified entry point with
/// the argument `arg`. Returns the new internal ID of the thread, or None on failure.
/// # Safety
/// Caller must ensure that the thread doesn't run out of stack, and that entry pointer refers to a
/// valid address to start executing code.
pub unsafe fn spawn(stack_size: usize, entry: usize, arg: usize) -> Option<u32> {
    let stack_layout = Layout::from_size_align(stack_size, STACK_ALIGN).unwrap();
    let stack_base = crate::alloc::global_alloc(stack_layout);
    let (tls_set, tls_base, tls_len, tls_align) = crate::rt1::new_thread_tls().unwrap();
    let tls_layout = Layout::from_size_align(tls_len, tls_align).unwrap();
    let args = ThreadSpawnArgs::new(
        entry,
        stack_base as usize,
        stack_size,
        tls_set,
        arg,
        ThreadSpawnFlags::empty(),
        None,
    );
    let slot = crate::slot::global_allocate().or_else(|| {
        crate::alloc::global_free(stack_base, stack_layout);
        crate::alloc::global_free(tls_base, tls_layout);
        None
    })?;
    THREADS_LOCK.lock();
    let res = crate::syscall::sys_spawn(args);
    if let Ok(objid) = res {
        let mapres = crate::syscall::sys_object_map(
            None,
            objid,
            slot,
            Protections::READ | Protections::WRITE,
            MapFlags::empty(),
        );
        if mapres.is_ok() {
            let (base, _) = crate::slot::to_vaddr_range(slot);
            let internal_id = new_thread(
                objid,
                base as *mut ThreadRepr,
                tls_base,
                tls_len,
                tls_align,
                stack_base,
                stack_size,
                slot,
            );
            THREADS_LOCK.unlock();
            return Some(internal_id);
        }
    }
    THREADS_LOCK.unlock();
    crate::alloc::global_free(stack_base, stack_layout);
    crate::alloc::global_free(tls_base, tls_layout);
    crate::slot::global_release(slot);
    None
}

/// Wait until the specified thread terminates.
/// # Safety
/// The thread ID must be a valid thread ID.
pub unsafe fn join(id: u32) {
    THREADS_LOCK.lock();
    loop {
        let slice = core::slice::from_raw_parts(THREADS, THREADS_LEN);
        let repr = slice[id as usize].get_repr();
        if repr.status.load(Ordering::SeqCst) == 0 {
            let ts = crate::syscall::ThreadSync::new_sleep(crate::syscall::ThreadSyncSleep::new(
                crate::syscall::ThreadSyncReference::Virtual(&repr.status),
                0,
                crate::syscall::ThreadSyncOp::Equal,
                crate::syscall::ThreadSyncFlags::empty(),
            ));
            THREADS_LOCK.unlock();
            let _ = crate::syscall::sys_thread_sync(&mut [ts], None);
            THREADS_LOCK.lock();
        } else {
            break;
        }
    }
    release_thread(id);
    THREADS_LOCK.unlock();
}

/// Exit the current thread.
pub fn exit() -> ! {
    crate::syscall::sys_thread_exit(0, ptr::null_mut());
}
