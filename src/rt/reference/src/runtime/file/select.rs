use std::collections::HashMap;

use secgate::TwzError;
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_rt_abi::{
    bindings::{fd_set, wait_kind, WAIT_READ, WAIT_WRITE},
    fd::RawFd,
};

use crate::runtime::{
    file::{get_fd_slots, FileDesc},
    ReferenceRuntime,
};

pub struct FdSet {
    set: *mut libc::fd_set,
    nfd: usize,
}

impl FdSet {
    unsafe fn new(set: *mut fd_set, nfd: usize) -> Self {
        Self {
            set: set.cast(),
            nfd,
        }
    }

    fn insert(&self, fd: i32) {
        assert!((fd as usize) < self.nfd);
        unsafe { libc::FD_SET(fd, self.set) };
    }

    fn remove(&self, fd: i32) {
        assert!((fd as usize) < self.nfd);
        unsafe { libc::FD_CLR(fd, self.set) };
    }

    fn contains(&self, fd: i32) -> bool {
        assert!((fd as usize) < self.nfd);
        unsafe { libc::FD_ISSET(fd, self.set) }
    }
}

pub struct SelectFds {
    set: HashMap<(RawFd, wait_kind), FileDesc>,
}

pub struct SelectState {
    pub read: FdSet,
    pub write: FdSet,
    pub _except: FdSet,
    pub fds: SelectFds,
    pub timeout: Option<std::time::Duration>,
}

impl SelectState {
    pub fn new(
        nfds: usize,
        read: FdSet,
        write: FdSet,
        except: FdSet,
        timeout: Option<std::time::Duration>,
    ) -> Result<Self, TwzError> {
        let mut fds = SelectFds {
            set: HashMap::new(),
        };
        let binding = get_fd_slots().lock().unwrap();
        for fd in 0..nfds {
            let fd = fd as RawFd;
            if read.contains(fd) {
                fds.set.insert(
                    (fd, WAIT_READ),
                    binding
                        .get(fd as usize)
                        .cloned()
                        .ok_or(TwzError::BAD_HANDLE)?,
                );
                read.remove(fd);
            }
            if write.contains(fd) {
                fds.set.insert(
                    (fd, WAIT_WRITE),
                    binding
                        .get(fd as usize)
                        .cloned()
                        .ok_or(TwzError::BAD_HANDLE)?,
                );
                write.remove(fd);
            }
            if except.contains(fd) {
                except.remove(fd);
                // Unsupported for now
            }
        }
        Ok(Self {
            fds,
            timeout,
            read,
            write,
            _except: except,
        })
    }

    fn wait(&self) -> Result<usize, TwzError> {
        let mut ready = 0;

        let maybe_mark_ready = |wp: &ThreadSync, kind: wait_kind, fd: RawFd| -> bool {
            let is_ready = wp.ready();
            if is_ready {
                match kind {
                    w if w == WAIT_READ => self.read.insert(fd),
                    w if w == WAIT_WRITE => self.write.insert(fd),
                    _ => {}
                }
            }
            is_ready
        };

        let (fds, mut waits): (Vec<_>, Vec<_>) = self
            .fds
            .set
            .iter()
            .filter_map(|((fd, kind), fd_desc)| {
                let wp = ThreadSync::new_sleep(fd_desc.file.waitpoint(*kind).ok()?);
                if maybe_mark_ready(&wp, *kind, *fd) {
                    ready += 1;
                }
                Some(((fd, *kind, fd_desc), wp))
            })
            .unzip();

        if ready > 0 {
            return Ok(ready);
        }

        sys_thread_sync(&mut waits, self.timeout)?;

        for ((fd, kind, _), wp) in fds.into_iter().zip(waits.into_iter()) {
            if maybe_mark_ready(&wp, kind, *fd) {
                ready += 1;
            }
        }

        Ok(ready)
    }
}

impl ReferenceRuntime {
    pub fn select(
        &self,
        nfd: usize,
        read: *mut fd_set,
        write: *mut fd_set,
        except: *mut fd_set,
        timeout: Option<std::time::Duration>,
    ) -> Result<usize, TwzError> {
        let state = SelectState::new(
            nfd,
            unsafe { FdSet::new(read, nfd) },
            unsafe { FdSet::new(write, nfd) },
            unsafe { FdSet::new(except, nfd) },
            timeout,
        )?;
        state.wait()
    }
}
