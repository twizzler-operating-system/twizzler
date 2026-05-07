use secgate::TwzError;
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_rt_abi::bindings::{wait_kind, WAIT_READ, WAIT_WRITE};

use crate::runtime::{file::get_fd_slots, ReferenceRuntime};

pub struct PollState<'a> {
    pub timeout: Option<std::time::Duration>,
    pub fds: &'a mut [twizzler_rt_abi::bindings::pollfd],
    pub wps: Vec<ThreadSync>,
    pub info: Vec<(usize, wait_kind)>,
    pub ready: usize,
}

fn events_to_wait_kind_iter(events: libc::c_short) -> impl Iterator<Item = wait_kind> {
    let mut events = events;
    std::iter::from_fn(move || {
        if events == 0 {
            None
        } else {
            if events & libc::POLLIN != 0 {
                events &= !libc::POLLIN;
                Some(WAIT_READ)
            } else if events & libc::POLLOUT != 0 {
                events &= !libc::POLLOUT;
                Some(WAIT_WRITE)
            } else {
                // We ignore other event types for now
                events = 0;
                None
            }
        }
    })
}

fn wait_kind_to_poll_revents(kind: wait_kind) -> libc::c_short {
    if kind == WAIT_READ {
        libc::POLLIN
    } else if kind == WAIT_WRITE {
        libc::POLLOUT
    } else {
        0
    }
}

impl<'a> PollState<'a> {
    pub fn new(
        fds: &'a mut [twizzler_rt_abi::bindings::pollfd],
        timeout: Option<std::time::Duration>,
    ) -> Result<Self, TwzError> {
        let slots = get_fd_slots().lock().unwrap();
        let mut wps = Vec::with_capacity(fds.len());
        let mut info = Vec::with_capacity(fds.len());
        let mut ready = 0;
        fds.iter_mut().enumerate().try_for_each(|(idx, fd)| {
            let file_desc = slots
                .get(fd.fd as usize)
                .ok_or(TwzError::BAD_HANDLE)?
                .clone();
            tracing::debug!("PollState::new: fd={}, events={:#x}", fd.fd, fd.events,);
            fd.revents = 0;
            for wk in events_to_wait_kind_iter(fd.events) {
                if let Some(wp) = file_desc.file.waitpoint(wk).ok() {
                    if wp.1 || wp.0.ready() {
                        if fd.revents == 0 {
                            ready += 1;
                        }
                        fd.revents |= wait_kind_to_poll_revents(wk);
                    } else {
                        wps.push(ThreadSync::new_sleep(wp.0));
                        info.push((idx, wk));
                    }
                }
            }
            Ok::<_, TwzError>(())
        })?;

        Ok(Self {
            timeout,
            fds,
            wps,
            ready,
            info,
        })
    }

    fn wait(&mut self) -> Result<usize, TwzError> {
        tracing::debug!("PollState::wait: initial ready={}", self.ready,);
        if self.ready > 0 {
            return Ok(self.ready);
        }
        let r = sys_thread_sync(self.wps.as_mut_slice(), self.timeout);
        tracing::debug!("PollState::wait: sys_thread_sync returned {:?}", r);
        match r {
            Ok(_) => {}
            Err(TwzError::TIMED_OUT) => {}
            Err(e) => return Err(e),
        }

        for (wp, (idx, wk)) in self.wps.iter().zip(self.info.iter()) {
            if wp.ready() {
                if self.fds[*idx].revents == 0 {
                    self.ready += 1;
                }
                self.fds[*idx].revents |= wait_kind_to_poll_revents(*wk);
            }
        }
        tracing::debug!("PollState::wait: final ready={}", self.ready,);

        Ok(self.ready)
    }
}

impl ReferenceRuntime {
    pub fn poll(
        &self,
        fds: &mut [twizzler_rt_abi::bindings::pollfd],
        timeout: Option<std::time::Duration>,
    ) -> Result<usize, TwzError> {
        self.ppoll(fds, timeout, std::ptr::null())
    }

    pub fn ppoll(
        &self,
        fds: &mut [twizzler_rt_abi::bindings::pollfd],
        timeout: Option<std::time::Duration>,
        _sigmask: *const libc::sigset_t,
    ) -> Result<usize, TwzError> {
        let mut ps = PollState::new(fds, timeout)?;
        ps.wait()
    }
}
