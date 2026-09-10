use std::{
    cell::RefCell,
    sync::{atomic::AtomicU64, Arc},
};

use secgate::TwzError;
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_rt_abi::{
    bindings::{fd_set, wait_kind, WAIT_READ, WAIT_WRITE},
    fd::RawFd,
};

use crate::runtime::{
    file::{get_fd_slots, FdImpl},
    ReferenceRuntime,
};

#[derive(Debug)]
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

/// The buffers one `select` call fills, reused across calls on the same thread -- the same
/// treatment as `poll`'s `PollScratch`, for the same reason. Emptied on release so an idle
/// thread holds no descriptors or `keepalive` `Arc`s from a call that already returned.
#[derive(Default)]
struct SelectScratch {
    fds: Vec<(RawFd, wait_kind, FdImpl)>,
    waits: Vec<ThreadSync>,
    info: Vec<(RawFd, wait_kind)>,
    keepalives: Vec<Option<Arc<AtomicU64>>>,
}

impl SelectScratch {
    fn clear(&mut self) {
        self.fds.clear();
        self.waits.clear();
        self.info.clear();
        self.keepalives.clear();
    }
}

thread_local! {
    static SELECT_SCRATCH: RefCell<SelectScratch> = RefCell::new(SelectScratch::default());
}

struct ScratchGuard(Option<SelectScratch>);

impl ScratchGuard {
    fn take() -> Self {
        let mut s = SELECT_SCRATCH.with(|c| core::mem::take(&mut *c.borrow_mut()));
        s.clear();
        Self(Some(s))
    }

    fn get(&mut self) -> &mut SelectScratch {
        // Unwrap-Ok: only `drop` takes the value, and that is the end of this guard's life.
        self.0.as_mut().unwrap()
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        if let Some(mut s) = self.0.take() {
            s.clear();
            SELECT_SCRATCH.with(|c| *c.borrow_mut() = s);
        }
    }
}

pub struct SelectState<'a> {
    pub read: FdSet,
    pub write: FdSet,
    pub _except: FdSet,
    pub fds: &'a mut Vec<(RawFd, wait_kind, FdImpl)>,
    pub timeout: Option<std::time::Duration>,
}

impl<'a> SelectState<'a> {
    pub fn new(
        nfds: usize,
        read: FdSet,
        write: FdSet,
        except: FdSet,
        timeout: Option<std::time::Duration>,
        fds: &'a mut Vec<(RawFd, wait_kind, FdImpl)>,
    ) -> Result<Self, TwzError> {
        let binding = get_fd_slots().read().unwrap();
        for fd in 0..nfds {
            let fd = fd as RawFd;
            if read.contains(fd) {
                fds.push((
                    fd,
                    WAIT_READ,
                    binding.io_parts(fd as usize).ok_or(TwzError::BAD_HANDLE)?.0,
                ));
                read.remove(fd);
            }
            if write.contains(fd) {
                fds.push((
                    fd,
                    WAIT_WRITE,
                    binding.io_parts(fd as usize).ok_or(TwzError::BAD_HANDLE)?.0,
                ));
                write.remove(fd);
            }
            if except.contains(fd) {
                except.remove(fd);
                // Unsupported for now
            }
        }
        tracing::debug!("SelectState::new: nfds={}, timeout={:?}", nfds, timeout);
        Ok(Self {
            fds,
            timeout,
            read,
            write,
            _except: except,
        })
    }

    fn wait(
        &self,
        waits: &mut Vec<ThreadSync>,
        info: &mut Vec<(RawFd, wait_kind)>,
        keepalives: &mut Vec<Option<Arc<AtomicU64>>>,
    ) -> Result<usize, TwzError> {
        let mut ready = 0;

        let maybe_mark_ready =
            |wp: &ThreadSync, kind: wait_kind, fd: RawFd, fd_is_ready: bool| -> bool {
                let is_ready = wp.ready() || fd_is_ready;
                if is_ready {
                    match kind {
                        w if w == WAIT_READ => self.read.insert(fd),
                        w if w == WAIT_WRITE => self.write.insert(fd),
                        _ => {}
                    }
                }
                is_ready
            };

        for (fd, kind, fd_desc) in self.fds.iter() {
            let Ok(wp) = fd_desc.waitpoint(*kind) else {
                continue;
            };
            let sleep = ThreadSync::new_sleep(wp.sleep);
            if maybe_mark_ready(&sleep, *kind, *fd, wp.ready) {
                ready += 1;
            }
            info.push((*fd, *kind));
            waits.push(sleep);
            // Must be held alive for as long as `waits` may still be read (through the
            // sys_thread_sync call below) -- see WaitpointResult::keepalive.
            keepalives.push(wp.keepalive);
        }
        tracing::debug!("SelectState::wait: initial ready={}", ready,);

        if ready > 0 {
            return Ok(ready);
        }

        match sys_thread_sync(waits, self.timeout) {
            Ok(_) => {}
            Err(TwzError::TIMED_OUT) => {}
            Err(e) => return Err(e),
        }

        for ((fd, kind), wp) in info.iter().zip(waits.iter()) {
            if maybe_mark_ready(wp, *kind, *fd, false) {
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
        let mut guard = ScratchGuard::take();
        let SelectScratch {
            fds,
            waits,
            info,
            keepalives,
        } = guard.get();
        let state = SelectState::new(
            nfd,
            unsafe { FdSet::new(read, nfd) },
            unsafe { FdSet::new(write, nfd) },
            unsafe { FdSet::new(except, nfd) },
            timeout,
            fds,
        )?;
        state.wait(waits, info, keepalives)
    }
}
