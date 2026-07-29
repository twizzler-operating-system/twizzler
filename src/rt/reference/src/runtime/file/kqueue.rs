use std::{sync::Mutex, time::Duration};

use secgate::TwzError;
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_rt_abi::{
    bindings::{
        kevent, kevent_filter, wait_kind, EVFILT_READ, EVFILT_WRITE, EV_ADD, EV_DELETE,
        EV_DISABLE, EV_ENABLE, EV_ERROR, EV_ONESHOT, WAIT_READ, WAIT_WRITE,
    },
    error::ArgumentError,
    fd::{FdFlags, FdInfo, FdKind, RawFd},
    io::{Endpoint, IoFlags},
    Result,
};

use crate::runtime::{file::get_fd_slots, file::Fd, ReferenceRuntime};

fn filter_to_wait_kind(filter: kevent_filter) -> Option<wait_kind> {
    if filter == EVFILT_READ {
        Some(WAIT_READ)
    } else if filter == EVFILT_WRITE {
        Some(WAIT_WRITE)
    } else {
        None
    }
}

fn error_event(change: &kevent) -> kevent {
    kevent {
        flags: EV_ERROR,
        data: TwzError::from(ArgumentError::InvalidArgument).raw() as isize,
        ..*change
    }
}

#[derive(Clone, Copy)]
struct KqueueEntry {
    ident: RawFd,
    filter: kevent_filter,
    enabled: bool,
    oneshot: bool,
    fflags: u32,
    udata: *mut std::ffi::c_void,
}

// Only ever touched behind KqueueFile::entries' Mutex.
unsafe impl Send for KqueueEntry {}

pub struct KqueueFile {
    entries: Mutex<Vec<KqueueEntry>>,
}

impl KqueueFile {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Apply a changelist to the persistent registration set, pushing an EV_ERROR event for any
    /// entry that couldn't be applied (unsupported filter, or ENABLE/DISABLE/DELETE targeting a
    /// registration that doesn't exist).
    fn apply_changes(&self, changelist: &[kevent], errors: &mut Vec<kevent>) {
        let mut entries = self.entries.lock().unwrap();
        for change in changelist {
            if filter_to_wait_kind(change.filter).is_none() {
                errors.push(error_event(change));
                continue;
            }
            let pos = entries
                .iter()
                .position(|e| e.ident == change.ident as RawFd && e.filter == change.filter);

            if change.flags & EV_DELETE != 0 {
                match pos {
                    Some(pos) => {
                        entries.remove(pos);
                    }
                    None => errors.push(error_event(change)),
                }
                continue;
            }

            if change.flags & EV_ADD != 0 {
                let entry = KqueueEntry {
                    ident: change.ident as RawFd,
                    filter: change.filter,
                    enabled: change.flags & EV_DISABLE == 0,
                    oneshot: change.flags & EV_ONESHOT != 0,
                    fflags: change.fflags,
                    udata: change.udata,
                };
                match pos {
                    Some(pos) => entries[pos] = entry,
                    None => entries.push(entry),
                }
                continue;
            }

            // Bare EV_ENABLE/EV_DISABLE without EV_ADD, targeting an existing registration.
            match pos {
                Some(pos) => {
                    if change.flags & EV_ENABLE != 0 {
                        entries[pos].enabled = true;
                    }
                    if change.flags & EV_DISABLE != 0 {
                        entries[pos].enabled = false;
                    }
                }
                None => errors.push(error_event(change)),
            }
        }
    }
}

impl Fd for KqueueFile {
    fn read(
        &self,
        _buf: &mut [u8],
        _flags: IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut Endpoint>,
    ) -> Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn write(
        &self,
        _buf: &[u8],
        _flags: IoFlags,
        _offset: Option<u64>,
        _to: Option<&Endpoint>,
    ) -> Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn stat(&self) -> Result<FdInfo> {
        Ok(FdInfo {
            size: 0,
            flags: FdFlags::empty(),
            kind: FdKind::Other,
            id: 0,
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: 0,
        })
    }

    fn as_kqueue(&self) -> Option<&KqueueFile> {
        Some(self)
    }
}

impl ReferenceRuntime {
    pub fn kevent(
        &self,
        kq: RawFd,
        changelist: &[kevent],
        eventlist: &mut [kevent],
        timeout: Option<Duration>,
    ) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(kq as usize)
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        let kqf = file_desc
            .file
            .as_kqueue()
            .ok_or(ArgumentError::InvalidArgument)?;

        let mut out_count = 0;
        let mut errors = Vec::new();
        kqf.apply_changes(changelist, &mut errors);
        for e in errors {
            if out_count >= eventlist.len() {
                break;
            }
            eventlist[out_count] = e;
            out_count += 1;
        }

        if out_count >= eventlist.len() {
            return Ok(out_count);
        }

        // Snapshot the currently-enabled registrations. We build the wait set from this snapshot
        // (rather than holding `entries` locked across the blocking syscall below), and reconcile
        // EV_ONESHOT removal against it afterward.
        let snapshot: Vec<KqueueEntry> = kqf
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.enabled)
            .copied()
            .collect();

        let slots = get_fd_slots().lock().unwrap();
        let mut wps = Vec::new();
        let mut info = Vec::new();
        let mut ready = Vec::new();
        for (idx, entry) in snapshot.iter().enumerate() {
            let Some(wk) = filter_to_wait_kind(entry.filter) else {
                continue;
            };
            let Some(fd) = slots.get(entry.ident as usize) else {
                continue;
            };
            let Ok(wp) = fd.file.waitpoint(wk) else {
                continue;
            };
            if wp.1 || wp.0.ready() {
                ready.push(idx);
            } else {
                wps.push(ThreadSync::new_sleep(wp.0));
                info.push(idx);
            }
        }
        drop(slots);

        if ready.is_empty() {
            match sys_thread_sync(&mut wps, timeout) {
                Ok(_) => {}
                Err(TwzError::TIMED_OUT) => {}
                Err(e) => return Err(e),
            }
            for (wp, idx) in wps.iter().zip(info.iter()) {
                if wp.ready() {
                    ready.push(*idx);
                }
            }
        }

        let mut oneshot_fired = Vec::new();
        for idx in ready {
            if out_count >= eventlist.len() {
                break;
            }
            let entry = &snapshot[idx];
            eventlist[out_count] = kevent {
                ident: entry.ident as usize,
                filter: entry.filter,
                flags: 0,
                fflags: entry.fflags,
                data: 0,
                udata: entry.udata,
            };
            out_count += 1;
            if entry.oneshot {
                oneshot_fired.push((entry.ident, entry.filter));
            }
        }

        if !oneshot_fired.is_empty() {
            kqf.entries
                .lock()
                .unwrap()
                .retain(|e| !oneshot_fired.contains(&(e.ident, e.filter)));
        }

        Ok(out_count)
    }
}
