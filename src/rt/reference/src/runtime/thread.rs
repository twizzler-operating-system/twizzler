//! Implements thread management routines.

use std::{
    ffi::c_void,
    sync::atomic::{AtomicU64, Ordering},
};

use dynlink::tls::Tcb;
use twizzler_abi::syscall::{
    sys_thread_send_message, sys_thread_sync, sys_thread_yield, ThreadSync, ThreadSyncFlags,
    ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_rt_abi::{
    bindings::{thread_info, twz_error},
    error::{ArgumentError, TwzError},
    thread::{ThreadSpawnArgs, TlsIndex},
    Result,
};

use super::ReferenceRuntime;
use crate::{
    preinit_println,
    runtime::{
        thread::{internal::InternalThread, mgr::ThreadManager},
        RuntimeState,
    },
};

mod internal;
mod mgr;
mod tcb;
pub(crate) use tcb::{libc_init_tcb, with_current_thread, TLS_GEN_MGR};

const MIN_STACK_ALIGN: usize = 128;

static THREAD_MGR: ThreadManager = ThreadManager::new();

/// Counts signal handlers that ran on this thread and that should interrupt a blocking operation.
///
/// Nothing here bumps it. POSIX interrupts a blocking call only for a caught handler with
/// `SA_RESTART` clear, and the handler table lives in libc, so libc calls `twz_rt_interrupt_bump`
/// when that case applies. The runtime's own default upcall handling either terminates the thread
/// or ignores the signal, and neither should interrupt anything.
#[thread_local]
static INTERRUPT_GEN: AtomicU64 = AtomicU64::new(0);

impl ReferenceRuntime {
    pub fn available_parallelism(&self) -> core::num::NonZeroUsize {
        twizzler_abi::syscall::sys_info().cpu_count()
    }

    pub fn gc_threads(&self) {
        THREAD_MGR.gc();
    }

    pub fn futex_wait(
        &self,
        futex: &core::sync::atomic::AtomicU32,
        expected: u32,
        timeout: Option<core::time::Duration>,
    ) -> twz_error {
        let _g = crate::runtime::file::namestats::FUTEX_WAIT.guard();
        // No need to wait if the value already changed.
        if futex.load(core::sync::atomic::Ordering::Relaxed) != expected {
            return 0;
        }

        let r = sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual32(futex),
                expected as u64,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            timeout,
        );

        match r {
            Err(e) => return e.raw(),
            _ => return 0,
        }
    }

    /// Wake up to `count` waiters, reporting how many were actually woken.
    ///
    /// The count comes from the *per-operation* result, not the syscall's return value: that one is
    /// how many operations were immediately ready, and the single-wake fast path returns 1 for a
    /// wake that found nobody. `ThreadSync::Wake`'s own result is the thread count.
    pub fn futex_wake(&self, futex: &core::sync::atomic::AtomicU32, count: usize) -> Result<usize> {
        use crate::runtime::file::namestats;
        let _g = namestats::FUTEX_WAKE.guard();
        let mut ops = [ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            count,
        ))];
        sys_thread_sync(&mut ops, None)?;
        let woken = ops[0].get_result();
        if matches!(woken, Ok(0)) {
            namestats::WAKE_NOBODY.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        woken
    }

    pub fn yield_now(&self) {
        sys_thread_yield()
    }

    pub fn set_name(&self, name: &std::ffi::CStr) {
        with_current_thread(|cur| {
            let repr_id = THREAD_MGR.with_internal(cur.id(), |th| {
                th.set_name(name);
                th.objid()
            });
            // Mirror the name into a note on the repr object so kernel-side diagnostics (the
            // hang wait-table dump) can label the thread.
            if let Some(repr_id) = repr_id {
                if repr_id.raw() != 0 {
                    let _ = twizzler_abi::syscall::sys_object_add_note(repr_id, name.to_bytes());
                }
            }
        })
    }

    pub fn get_name(&self, _tcb: *const c_void, name: &mut [u8]) -> usize {
        // TODO: if _tcb is non null and points to a different thread than our own, read that
        // thread's name.
        with_current_thread(|cur| {
            THREAD_MGR
                .with_internal(cur.id(), |th| th.get_name(name))
                .unwrap_or_else(|| {
                    name.fill(0);
                    0
                })
        })
    }

    pub fn sleep(&self, duration: std::time::Duration) {
        let _ = sys_thread_sync(&mut [], Some(duration));
    }

    pub fn interrupt_word(&self) -> *const AtomicU64 {
        &raw const INTERRUPT_GEN
    }

    pub fn interrupt_bump(&self) {
        INTERRUPT_GEN.fetch_add(1, Ordering::Release);
    }

    /// Post a signal to another thread of this compartment.
    ///
    /// The kernel side does the work that makes this different from `kill()`: the message lands on
    /// that thread's own pending mask, and `SendMessage` resets its sync sleep, so a target parked
    /// in `sys_thread_sync` wakes rather than waiting out an unrelated wakeup. Delivery itself
    /// happens on the target's next return to user, as a Mailbox upcall.
    ///
    /// The signal number is the mailbox bit index, and the kernel rejects 0 and anything from 64
    /// up, so reject those here rather than handing over an argument that will be refused.
    pub fn thread_signal(&self, id: u32, signal: u64) -> Result<()> {
        if signal == 0 || signal >= 64 {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let repr = THREAD_MGR
            .with_internal(id, |t| t.objid())
            .ok_or(TwzError::NOT_FOUND)?;
        // A thread published before its spawn gate returned has no repr id yet. It cannot be
        // holding a blocking call we need to interrupt, and 0 would name no thread at all.
        if repr.raw() == 0 {
            return Err(TwzError::NOT_FOUND);
        }
        sys_thread_send_message(repr, signal, 0)
    }

    pub fn tls_get_addr(&self, index: &TlsIndex) -> Option<*mut u8> {
        {
            let tp: &Tcb<()> = unsafe {
                match dynlink::tls::get_current_thread_control_block().as_ref() {
                    Some(tp) => tp,
                    None => {
                        preinit_println!("failed to locate TLS data");
                        self.abort();
                    }
                }
            };

            if let Some(addr) = tp.get_addr(index) {
                return Some(addr);
            }
        }
        // Slow path: the module ID is beyond this thread's DTV. If a library with TLS was
        // loaded after this thread's region was built, the compartment's template advanced;
        // catch up and retry.
        if tcb::upgrade_current_thread_dtv() {
            let tp: &Tcb<()> =
                unsafe { dynlink::tls::get_current_thread_control_block().as_ref()? };
            return tp.get_addr(index);
        }
        None
    }

    pub fn spawn(&self, args: ThreadSpawnArgs) -> Result<(u32, *mut c_void)> {
        self.impl_spawn(args)
    }

    pub fn join(&self, id: u32, timeout: Option<std::time::Duration>) -> Result<()> {
        self.impl_join(id, timeout)
    }

    pub fn thread_get_info(&self, id: Option<u32>) -> thread_info {
        let make_info = |th: &InternalThread| -> thread_info {
            thread_info {
                id: th.id,
                tcb: th.tls.cast(),
                objid: th.objid().raw(),
            }
        };
        let id = match id {
            Some(id) => id,
            None => {
                // Before READY the caller may still be on the bootstrap TLS, whose
                // RuntimeThreadControl was never constructed -- its lock word is garbage and
                // reading it aborts. libc's gettid reaches here from any early mutex.
                //
                // Report 1, not 0. mlibc's `this_tid` (tid.hpp) treats 0 as the "unpopulated"
                // sentinel *and caches it*, so returning 0 makes `this_tid() == 0`, which runs the
                // mutex protocol with tid 0: the ownerMask CAS degenerates to 0->0 and lock.hpp's
                // `__ensure((state & ownerMask) == this_tid())` aborts (ud2) -- a monitor-bootstrap
                // crash. 1 is not a placeholder: pre-READY the only thread alive is the core thread
                // (mgr.rs `next_id: 2 // 0 reserved, 1 is the core thread`), so 1 is the id this
                // caller will genuinely report after READY. A lock acquired pre-READY and released
                // post-READY therefore compares equal (both 1) -- the READY-straddle is closed by
                // construction, not hoped away.
                //
                // LOAD-BEARING ASSUMPTION: this is correct only while pre-READY is single-threaded.
                // If anything ever spawns a second thread before READY, both would report 1 and the
                // ownerMask compare would wrongly succeed -- silent corruption rather than a loud
                // abort. Nothing spawns pre-READY today; keep it that way.
                if !self.state().contains(RuntimeState::READY) {
                    return thread_info {
                        id: 1,
                        tcb: core::ptr::null_mut(),
                        objid: 0,
                    };
                }
                with_current_thread(|cur| cur.id())
            }
        };
        THREAD_MGR
            .with_internal(id, make_info)
            .unwrap_or(thread_info {
                id,
                tcb: core::ptr::null_mut(),
                objid: 0,
            })
    }
}
