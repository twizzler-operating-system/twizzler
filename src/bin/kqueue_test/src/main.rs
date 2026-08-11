use std::time::Duration;

use twizzler_rt_abi::{
    bindings::{
        kevent, option_duration, twz_rt_fd_kevent, EVFILT_READ, EV_ADD, EV_DELETE, EV_ONESHOT,
        OPEN_FLAG_READ, OPEN_FLAG_WRITE,
    },
    fd::{twz_rt_fd_get_info, twz_rt_fd_open_kqueue, twz_rt_fd_open_pipe, RawFd},
    io::{twz_rt_fd_pread, twz_rt_fd_pwrite, IoCtx},
};

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

// Under `--test` libtest supplies its own entry point, so this one is unused there.
#[cfg(not(test))]
fn main() {
    test_basic_read_ready();
    test_oneshot_removed_after_firing();
    test_delete_removes_registration();
    println!("kqueue_test: all tests passed");
}
