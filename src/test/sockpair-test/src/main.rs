//! `socketpair(AF_UNIX, SOCK_STREAM)` as `UnixStream::pair` sees it, plus the two registry crates
//! whose upstream builds depend on runtime-side support: `errno` (links `errno_location`) and
//! `getrandom` 0.2 (`custom` backend). signal-hook's wakeup channel is exactly this pair with
//! one nonblocking end polled for readability, so that shape is covered explicitly.

use std::{
    io::{ErrorKind, Read, Write},
    net::Shutdown,
    os::unix::{io::AsRawFd, net::UnixStream},
    time::Duration,
};

// Run both as libtest cases under `--test` and from `main()` standalone; see kqueue_test.
#[cfg_attr(test, test)]
fn test_pair_roundtrip() {
    println!("test_pair_roundtrip");
    let (mut a, mut b) = UnixStream::pair().expect("socketpair");
    a.write_all(b"ping").unwrap();
    let mut buf = [0u8; 4];
    b.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ping");
    b.write_all(b"pong").unwrap();
    a.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"pong");
}

#[cfg_attr(test, test)]
fn test_pair_nonblocking_and_poll() {
    println!("test_pair_nonblocking_and_poll");
    let (mut a, mut b) = UnixStream::pair().expect("socketpair");
    b.set_nonblocking(true).unwrap();
    let mut buf = [0u8; 1];
    let err = b
        .read(&mut buf)
        .expect_err("empty nonblocking read must not block");
    assert_eq!(err.kind(), ErrorKind::WouldBlock);

    let mut pfd = libc::pollfd {
        fd: b.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut pfd, 1, 50) };
    assert_eq!(n, 0, "nothing readable before a write");

    a.write_all(b"x").unwrap();
    let n = unsafe { libc::poll(&mut pfd, 1, 5000) };
    assert_eq!(n, 1, "readable after the peer wrote");
    assert!(pfd.revents & libc::POLLIN != 0);
    assert_eq!(b.read(&mut buf).unwrap(), 1);
    assert_eq!(buf[0], b'x');
}

#[cfg_attr(test, test)]
fn test_pair_eof_on_drop_and_shutdown() {
    println!("test_pair_eof_on_drop_and_shutdown");
    let (a, mut b) = UnixStream::pair().expect("socketpair");
    drop(a);
    let mut buf = [0u8; 8];
    assert_eq!(b.read(&mut buf).unwrap(), 0, "EOF once the peer is gone");

    let (a, mut b) = UnixStream::pair().expect("socketpair");
    a.shutdown(Shutdown::Write).unwrap();
    assert_eq!(
        b.read(&mut buf).unwrap(),
        0,
        "EOF after the peer shut down its write side"
    );
    // The other direction is unaffected.
    b.write_all(b"still").unwrap();
    let mut a = a;
    let mut got = [0u8; 5];
    a.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"still");
}

#[cfg_attr(test, test)]
fn test_pair_fd_is_socket() {
    println!("test_pair_fd_is_socket");
    let (a, _b) = UnixStream::pair().expect("socketpair");
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(a.as_raw_fd(), &mut st) }, 0);
    assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFSOCK);
}

#[cfg_attr(test, test)]
fn test_errno_crate_reaches_libc_errno() {
    println!("test_errno_crate_reaches_libc_errno");
    errno::set_errno(errno::Errno(libc::EINVAL));
    assert_eq!(errno::errno().0, libc::EINVAL);
    assert_eq!(unsafe { libc::close(-1) }, -1);
    assert_eq!(
        errno::errno().0,
        libc::EBADF,
        "a failing libc call must be visible to the crate"
    );
    assert!(!errno::errno().to_string().is_empty());
}

#[cfg_attr(test, test)]
fn test_getrandom02_custom_backend() {
    println!("test_getrandom02_custom_backend");
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut a).expect("getrandom 0.2");
    getrandom::getrandom(&mut b).expect("getrandom 0.2");
    assert_ne!(a, [0u8; 32]);
    assert_ne!(a, b);
    let mut c = [0u8; 0];
    getrandom::getrandom(&mut c).expect("zero-length fill");
    let _ = Duration::ZERO;
}

#[cfg(not(test))]
fn main() {
    test_pair_roundtrip();
    test_pair_nonblocking_and_poll();
    test_pair_eof_on_drop_and_shutdown();
    test_pair_fd_is_socket();
    test_errno_crate_reaches_libc_errno();
    test_getrandom02_custom_backend();
    println!("sockpair_test: all passed");
}
