use std::time::{Duration, Instant};

use twizzler_rt_abi::{
    bindings::{
        kevent, option_duration, twz_rt_fd_kevent, EVFILT_READ, EVFILT_USER, EV_ADD, EV_CLEAR,
        EV_DELETE, EV_ERROR, EV_ONESHOT, EV_RECEIPT, NOTE_TRIGGER, OPEN_FLAG_READ, OPEN_FLAG_WRITE,
    },
    fd::{twz_rt_fd_get_info, twz_rt_fd_open_kqueue, twz_rt_fd_open_pipe, RawFd},
    io::{twz_rt_fd_pread, twz_rt_fd_pwrite, IoCtx},
};

// Not worth a libc dependency for one constant; this is the value mlibc uses.
const ENOENT: isize = 2;

fn make_pipe() -> (RawFd, RawFd) {
    let write_fd = twz_rt_fd_open_pipe(None, OPEN_FLAG_WRITE).expect("create pipe");
    let info = twz_rt_fd_get_info(write_fd).expect("stat pipe");
    let read_fd = twz_rt_fd_open_pipe(Some(info.id), OPEN_FLAG_READ).expect("open pipe read end");
    (read_fd, write_fd)
}

fn kevent_call(
    kq: RawFd,
    changelist: &[kevent],
    eventlist: &mut [kevent],
    timeout: Option<Duration>,
) -> usize {
    let timeout: option_duration = timeout.into();
    let res = unsafe {
        twz_rt_fd_kevent(
            kq,
            changelist.as_ptr(),
            changelist.len(),
            eventlist.as_mut_ptr(),
            eventlist.len(),
            timeout,
        )
    };
    let r: twizzler_rt_abi::Result<usize> = res.into();
    r.expect("kevent")
}

fn add_read_event(ident: RawFd, extra_flags: u16) -> kevent {
    kevent {
        ident: ident as usize,
        filter: EVFILT_READ,
        flags: EV_ADD | extra_flags,
        fflags: 0,
        data: 0,
        udata: core::ptr::null_mut(),
        ext: [0; 4],
    }
}

// These run both ways: as libtest cases under `--test` (which is how `unittest` reports them
// individually), and from `main()` when the binary is run standalone. A plain `#[test]` would
// remove them from the non-test build and leave `main()` calling nothing.
#[cfg_attr(test, test)]
fn test_basic_read_ready() {
    println!("test_basic_read_ready");
    let (read_fd, write_fd) = make_pipe();
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");

    let changes = [add_read_event(read_fd, 0)];
    let mut events = [kevent::default(); 4];

    // Nothing written yet -- should time out with zero events.
    let n = kevent_call(kq, &changes, &mut events, Some(Duration::from_millis(50)));
    assert_eq!(n, 0, "expected no events before any data is written");

    // Write, then expect the read event to fire (registration persists from the call above).
    twz_rt_fd_pwrite(write_fd, b"hi", &mut IoCtx::default()).expect("write to pipe");
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_secs(5)));
    assert_eq!(n, 1, "expected exactly one ready event");
    assert_eq!(events[0].ident, read_fd as usize);
    assert_eq!(events[0].filter, EVFILT_READ);

    // Drain it so later tests aren't confused by leftover data.
    let mut buf = [0u8; 8];
    let len = twz_rt_fd_pread(read_fd, &mut buf, &mut IoCtx::default()).expect("read from pipe");
    assert_eq!(&buf[0..len], b"hi");
}

#[cfg_attr(test, test)]
fn test_oneshot_removed_after_firing() {
    println!("test_oneshot_removed_after_firing");
    let (read_fd, write_fd) = make_pipe();
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");

    let changes = [add_read_event(read_fd, EV_ONESHOT)];
    let mut events = [kevent::default(); 4];

    twz_rt_fd_pwrite(write_fd, b"x", &mut IoCtx::default()).expect("write to pipe");
    let n = kevent_call(kq, &changes, &mut events, Some(Duration::from_secs(5)));
    assert_eq!(n, 1, "oneshot registration should fire once");

    let mut buf = [0u8; 8];
    twz_rt_fd_pread(read_fd, &mut buf, &mut IoCtx::default()).expect("read from pipe");

    // Write again -- since the registration was EV_ONESHOT, it should have been removed, so a
    // fresh wait (with no changelist) should time out rather than fire again.
    twz_rt_fd_pwrite(write_fd, b"y", &mut IoCtx::default()).expect("write to pipe");
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_millis(50)));
    assert_eq!(n, 0, "oneshot registration should not fire a second time");
}

#[cfg_attr(test, test)]
fn test_delete_removes_registration() {
    println!("test_delete_removes_registration");
    let (read_fd, write_fd) = make_pipe();
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");
    let mut events = [kevent::default(); 4];

    // Register, but nothing's ready yet -- just applying the change, so a short timeout is fine.
    kevent_call(
        kq,
        &[add_read_event(read_fd, 0)],
        &mut events,
        Some(Duration::from_millis(10)),
    );

    let del = kevent {
        ident: read_fd as usize,
        filter: EVFILT_READ,
        flags: EV_DELETE,
        fflags: 0,
        data: 0,
        udata: core::ptr::null_mut(),
        ext: [0; 4],
    };
    kevent_call(kq, &[del], &mut events, Some(Duration::from_millis(10)));

    twz_rt_fd_pwrite(write_fd, b"z", &mut IoCtx::default()).expect("write to pipe");
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_millis(50)));
    assert_eq!(n, 0, "deleted registration should not fire");
}

fn user_event(ident: usize, flags: u16, fflags: u32) -> kevent {
    kevent {
        ident,
        filter: EVFILT_USER,
        flags,
        fflags,
        data: 0,
        udata: core::ptr::null_mut(),
        ext: [0; 4],
    }
}

/// EV_RECEIPT must make a pure-registration call return without waiting. This is the exact shape
/// mio's Selector::register uses (eventlist sized to the changelist), and it passes a null -- i.e.
/// infinite -- timeout, so if receipts stop being emitted it blocks forever instead of registering.
#[cfg_attr(test, test)]
fn test_receipt_returns_without_waiting() {
    println!("test_receipt_returns_without_waiting");
    let (read_fd, _write_fd) = make_pipe();
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");

    let changes = [add_read_event(read_fd, EV_CLEAR | EV_RECEIPT)];
    let mut events = [kevent::default(); 1];

    // Nothing is ready, so without receipts this would burn the whole timeout.
    let start = Instant::now();
    let n = kevent_call(kq, &changes, &mut events, Some(Duration::from_secs(5)));
    let elapsed = start.elapsed();

    assert_eq!(n, 1, "expected one receipt per changelist entry");
    assert_ne!(events[0].flags & EV_ERROR, 0, "receipt must set EV_ERROR");
    assert_eq!(
        events[0].data, 0,
        "receipt for a change that applied cleanly must carry data == 0"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "EV_RECEIPT call waited {:?} instead of returning immediately",
        elapsed
    );
}

/// Deleting a registration that isn't there must report ENOENT specifically. mio's reregister
/// EV_DELETEs the direction it is dropping on every call and ignores exactly this errno; any other
/// value turns a routine reregister into a hard error.
#[cfg_attr(test, test)]
fn test_delete_missing_reports_enoent() {
    println!("test_delete_missing_reports_enoent");
    let (read_fd, _write_fd) = make_pipe();
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");

    let del = kevent {
        ident: read_fd as usize,
        filter: EVFILT_READ,
        flags: EV_DELETE | EV_RECEIPT,
        fflags: 0,
        data: 0,
        udata: core::ptr::null_mut(),
        ext: [0; 4],
    };
    let mut events = [kevent::default(); 1];
    let n = kevent_call(kq, &[del], &mut events, Some(Duration::from_millis(50)));

    assert_eq!(n, 1, "a failed change must produce a receipt");
    assert_ne!(events[0].flags & EV_ERROR, 0);
    assert_eq!(
        events[0].data, ENOENT,
        "deleting a missing registration must report ENOENT"
    );
}

/// EVFILT_USER backs mio::Waker, which is how a reactor gets unblocked from another thread.
#[cfg_attr(test, test)]
fn test_user_filter_trigger() {
    println!("test_user_filter_trigger");
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");
    let mut events = [kevent::default(); 4];

    // Registered but never triggered: must stay silent.
    let n = kevent_call(
        kq,
        &[user_event(7, EV_ADD | EV_CLEAR | EV_RECEIPT, 0)],
        &mut events,
        Some(Duration::from_millis(50)),
    );
    assert_eq!(n, 1, "expected just the receipt");
    assert_ne!(
        events[0].flags & EV_ERROR,
        0,
        "expected a receipt, not an event"
    );

    // mio's Waker::wake() is an EV_ADD carrying NOTE_TRIGGER.
    let n = kevent_call(
        kq,
        &[user_event(7, EV_ADD, NOTE_TRIGGER)],
        &mut events,
        Some(Duration::from_secs(5)),
    );
    assert_eq!(n, 1, "trigger should produce exactly one event");
    assert_eq!(events[0].ident, 7);
    assert_eq!(events[0].filter, EVFILT_USER);
    assert_eq!(
        events[0].flags & EV_ERROR,
        0,
        "expected an event, not a receipt"
    );

    // EVFILT_USER is clear-on-report: one trigger, one event.
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_millis(50)));
    assert_eq!(n, 0, "a consumed trigger must not fire again");
}

/// The trigger has to wake a kevent() that is already blocked, not just be visible to the next
/// call -- that is the whole point of a waker.
#[cfg_attr(test, test)]
fn test_user_filter_wakes_blocked_kevent() {
    println!("test_user_filter_wakes_blocked_kevent");
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");
    let mut events = [kevent::default(); 4];

    kevent_call(
        kq,
        &[user_event(9, EV_ADD | EV_CLEAR | EV_RECEIPT, 0)],
        &mut events,
        Some(Duration::from_millis(50)),
    );

    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        // Exactly mio's Waker::wake(): EV_RECEIPT with a one-entry eventlist, so the receipt
        // fills the list and this call returns before it can consume the trigger it just posted.
        let mut ev = [kevent::default(); 1];
        let n = kevent_call(
            kq,
            &[user_event(9, EV_ADD | EV_RECEIPT, NOTE_TRIGGER)],
            &mut ev,
            Some(Duration::from_millis(50)),
        );
        assert_eq!(n, 1);
        assert_ne!(
            ev[0].flags & EV_ERROR,
            0,
            "waker call consumed its own trigger"
        );
    });

    // Blocks with nothing ready until the thread above triggers.
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_secs(10)));
    trigger.join().expect("trigger thread panicked");

    assert_eq!(n, 1, "blocked kevent should have been woken by the trigger");
    assert_eq!(events[0].ident, 9);
    assert_eq!(events[0].filter, EVFILT_USER);
}

/// Pipes have no falling-edge waitpoint, so EV_CLEAR degrades to level-triggered for them. The
/// point of this test is that it degrades rather than going silent -- a suppressed registration
/// that can never re-arm would hang its consumer.
#[cfg_attr(test, test)]
fn test_clear_on_pipe_degrades_to_level() {
    println!("test_clear_on_pipe_degrades_to_level");
    let (read_fd, write_fd) = make_pipe();
    let kq = twz_rt_fd_open_kqueue(0).expect("open kqueue");
    let mut events = [kevent::default(); 4];

    kevent_call(
        kq,
        &[add_read_event(read_fd, EV_CLEAR)],
        &mut events,
        Some(Duration::from_millis(10)),
    );

    twz_rt_fd_pwrite(write_fd, b"a", &mut IoCtx::default()).expect("write to pipe");
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_secs(5)));
    assert_eq!(n, 1, "expected the read event");

    // Still unread, so a level-triggered fallback reports it again rather than falling silent.
    let n = kevent_call(kq, &[], &mut events, Some(Duration::from_secs(5)));
    assert_eq!(
        n, 1,
        "EV_CLEAR on a kind without falling-edge support must not go silent"
    );

    let mut buf = [0u8; 8];
    twz_rt_fd_pread(read_fd, &mut buf, &mut IoCtx::default()).expect("read from pipe");
}

// Under `--test` libtest supplies its own entry point, so this one is unused there.
#[cfg(not(test))]
fn main() {
    test_basic_read_ready();
    test_oneshot_removed_after_firing();
    test_delete_removes_registration();
    test_receipt_returns_without_waiting();
    test_delete_missing_reports_enoent();
    test_user_filter_trigger();
    test_user_filter_wakes_blocked_kevent();
    test_clear_on_pipe_degrades_to_level();
    println!("kqueue_test: all tests passed");
}
