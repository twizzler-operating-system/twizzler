//! Per-thread signal delivery, and whether it interrupts a blocking read.
//!
//! Two things this exercises, both of which used to be broken:
//!
//!  - `twz_rt_thread_signal` runs the handler on the *target* thread. mlibc's `sys_tgkill` ignored
//!    its `tid` and raised against the caller, so `pthread_kill(other, sig)` signalled the calling
//!    thread instead, silently, returning 0.
//!  - a thread blocked in `read()` comes out with `EINTR`. Nothing in the tree called
//!    `twz_rt_interrupt_bump`, so the interrupt-generation word never moved and every blocking call
//!    slept through every signal. The handler below stands in for the libc half of that contract:
//!    bump only for a caught handler that POSIX says should interrupt.
//!
//! Concretely this is what made rustc burn exactly one second per invocation in
//! `finish_ongoing_codegen`: the jobserver retires its helper thread with 100 x 10ms of
//! `pthread_kill(SIGUSR1)`, waiting for a blocked `read` to return `EINTR`.

use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::{Duration, Instant},
};

use twizzler_abi::syscall::sys_thread_active_sctx_id;
use twizzler_rt_abi::thread::{
    twz_rt_get_thread_info, twz_rt_interrupt_bump, twz_rt_thread_signal, THREAD_ID_SELF,
};

thread_local! {
    /// Set by the handler, read by the thread that was supposed to receive the signal. A
    /// thread-local is the whole point of the test: it records *where* the handler ran.
    static HANDLED_HERE: Cell<bool> = const { Cell::new(false) };
}

static HANDLER_RUNS: AtomicU32 = AtomicU32::new(0);
static READER_ID: AtomicU32 = AtomicU32::new(0);
static READER_ARMED: AtomicBool = AtomicBool::new(false);
/// Whether a *spawned* thread can take delivery at all. Split out because it is a different
/// failure from "delivered to the wrong thread": spawned threads used to be given Mailbox upcalls
/// in `CallSelf` mode with no self entry address, so nothing could ever reach them.
static READER_SELF_OK: AtomicBool = AtomicBool::new(false);
static READ_RESULT: AtomicU32 = AtomicU32::new(RESULT_PENDING);

const RESULT_PENDING: u32 = 0;
const RESULT_EINTR: u32 = 1;
const RESULT_DATA: u32 = 2;
const RESULT_OTHER: u32 = 3;

extern "C" fn on_signal(_sig: libc::c_int) {
    HANDLER_RUNS.fetch_add(1, Ordering::SeqCst);
    HANDLED_HERE.with(|h| h.set(true));
    // Stands in for libc: a caught handler with SA_RESTART clear interrupts blocking calls.
    twz_rt_interrupt_bump();
}

fn install_handler(sig: libc::c_int) {
    let mut act: libc::sigaction = unsafe { std::mem::zeroed() };
    act.sa_sigaction = on_signal as *const () as usize;
    // No SA_RESTART: this signal should interrupt, not restart.
    act.sa_flags = 0;
    let rc = unsafe { libc::sigaction(sig, &act, std::ptr::null_mut()) };
    assert_eq!(rc, 0, "sigaction failed");
}

fn self_id() -> u32 {
    twz_rt_get_thread_info(THREAD_ID_SELF).id
}

fn main() {
    install_handler(libc::SIGUSR1);

    // Phase 0: does a posted mailbox signal reach *any* handler? Aimed at the caller, so it does
    // not depend on the target lookup, only on delivery. Separates "posting works but nothing is
    // delivered" from "delivered, but to the wrong thread".
    let me = self_id();
    println!(
        "SIGTEST main thread id {} sctx {:#x}",
        me,
        sys_thread_active_sctx_id().raw()
    );
    match twz_rt_thread_signal(me, libc::SIGUSR1 as u64) {
        Ok(()) => {}
        Err(e) => println!("SIGTEST self-signal post failed: {}", e),
    }
    for _ in 0..50 {
        if HANDLER_RUNS.load(Ordering::SeqCst) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let self_delivered = HANDLER_RUNS.load(Ordering::SeqCst) > 0;
    let self_on_me = HANDLED_HERE.with(|h| h.get());
    println!(
        "SIGTEST self-signal delivered={} on_this_thread={}",
        self_delivered, self_on_me
    );
    HANDLER_RUNS.store(0, Ordering::SeqCst);
    HANDLED_HERE.with(|h| h.set(false));

    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        println!("SIGTEST FAIL pipe() failed");
        std::process::exit(1);
    }
    let (rfd, wfd) = (fds[0], fds[1]);

    let reader = std::thread::spawn(move || {
        let me = self_id();
        println!(
            "SIGTEST reader sees itself as id {} objid {:#x} sctx {:#x}",
            me,
            twz_rt_get_thread_info(THREAD_ID_SELF).objid,
            sys_thread_active_sctx_id().raw()
        );
        // Can a non-main thread take delivery at all? Separates "cannot deliver to this thread"
        // from "main resolved the wrong target for it".
        let _ = twz_rt_thread_signal(me, libc::SIGUSR1 as u64);
        for _ in 0..25 {
            if HANDLED_HERE.with(|h| h.get()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let self_ok = HANDLED_HERE.with(|h| h.get());
        println!("SIGTEST reader self-signal delivered={}", self_ok);
        READER_SELF_OK.store(self_ok, Ordering::SeqCst);
        HANDLED_HERE.with(|h| h.set(false));
        HANDLER_RUNS.store(0, Ordering::SeqCst);
        READER_ID.store(me, Ordering::SeqCst);
        READER_ARMED.store(true, Ordering::SeqCst);
        let mut buf = [0u8; 1];
        let n = unsafe { libc::read(rfd, buf.as_mut_ptr().cast(), 1) };
        let result = if n < 0 {
            let e = unsafe { *libc::__errno_location() };
            if e == libc::EINTR {
                RESULT_EINTR
            } else {
                println!("SIGTEST read failed with errno {}", e);
                RESULT_OTHER
            }
        } else {
            RESULT_DATA
        };
        READ_RESULT.store(result, Ordering::SeqCst);
        HANDLED_HERE.with(|h| h.get())
    });

    while !READER_ARMED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let target = READER_ID.load(Ordering::SeqCst);
    println!(
        "SIGTEST main resolves reader id {} to objid {:#x}",
        target,
        twz_rt_get_thread_info(target).objid
    );

    // Re-signal rather than signal once. A signal that lands before the reader samples its
    // interrupt generation cannot interrupt that read -- the same race POSIX has -- so keep
    // trying until the read reports back, and give up rather than hang.
    let deadline = Instant::now() + Duration::from_secs(10);
    while READ_RESULT.load(Ordering::SeqCst) == RESULT_PENDING && Instant::now() < deadline {
        if let Err(e) = twz_rt_thread_signal(target, libc::SIGUSR1 as u64) {
            println!("SIGTEST FAIL twz_rt_thread_signal: {}", e);
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let timed_out = READ_RESULT.load(Ordering::SeqCst) == RESULT_PENDING;
    if timed_out {
        // Unblock the reader so the process can exit and report, rather than wedging the suite.
        let byte = [0u8; 1];
        unsafe { libc::write(wfd, byte.as_ptr().cast(), 1) };
    }
    let handled_on_reader = reader.join().unwrap_or(false);
    let handled_on_main = HANDLED_HERE.with(|h| h.get());
    let result = READ_RESULT.load(Ordering::SeqCst);
    let runs = HANDLER_RUNS.load(Ordering::SeqCst);

    let mut failures = Vec::new();
    if timed_out {
        failures.push("blocking read was never interrupted");
    }
    if !READER_SELF_OK.load(Ordering::SeqCst) {
        failures.push("a spawned thread could not take delivery even from itself");
    }
    if runs == 0 {
        failures.push("handler never ran");
    }
    if handled_on_main {
        // The old behaviour exactly: the signal came back to the sender.
        failures.push("handler ran on the signalling thread");
    }
    if runs > 0 && !handled_on_reader {
        failures.push("handler did not run on the target thread");
    }
    if !timed_out && result != RESULT_EINTR {
        failures.push("read did not fail with EINTR");
    }

    unsafe {
        libc::close(rfd);
        libc::close(wfd);
    }

    if failures.is_empty() {
        println!("SIGTEST PASS handler ran on the target thread, read returned EINTR");
    } else {
        for f in &failures {
            println!("SIGTEST FAIL {}", f);
        }
        std::process::exit(1);
    }
}
