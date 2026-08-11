use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

use secgate::TwzError;
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_rt_abi::{
    bindings::{
        kevent, kevent_filter, wait_kind, EVFILT_READ, EVFILT_USER, EVFILT_WRITE, EV_ADD,
        EV_DELETE, EV_DISABLE, EV_ENABLE, EV_ERROR, EV_ONESHOT, EV_RECEIPT, NOTE_TRIGGER,
        WAIT_READ, WAIT_WRITE,
    },
    error::ArgumentError,
    fd::{FdFlags, FdInfo, FdKind, RawFd},
    io::{Endpoint, IoFlags},
    Result,
};

use crate::runtime::{
    file::{get_fd_slots, Fd},
    ReferenceRuntime,
};

fn filter_to_wait_kind(filter: kevent_filter) -> Option<wait_kind> {
    if filter == EVFILT_READ {
        Some(WAIT_READ)
    } else if filter == EVFILT_WRITE {
        Some(WAIT_WRITE)
    } else {
        None
    }
}

/// Build the eventlist receipt for a changelist entry: `errno` describes why the change failed, or
/// is 0 for the success receipt a change gets when it asked for EV_RECEIPT. `data` is an errno
/// rather than a TwzError so that libc callers (see syms.rs's kevent) can compare it against
/// ENOENT/EBADF the way they would on BSD.
fn receipt_event(change: &kevent, errno: i32) -> kevent {
    kevent {
        flags: EV_ERROR,
        data: errno as isize,
        ..*change
    }
}

#[derive(Clone, Copy)]
struct KqueueEntry {
    // A descriptor for the readiness filters; an arbitrary caller-chosen value for EVFILT_USER.
    ident: usize,
    filter: kevent_filter,
    enabled: bool,
    oneshot: bool,
    fflags: u32,
    udata: *mut std::ffi::c_void,
    // EVFILT_USER only: a NOTE_TRIGGER has been delivered and not yet reported.
    triggered: bool,
    // Bumped on every EV_ADD for this (ident, filter). Lets EV_ONESHOT reconciliation
    // (see kevent()) tell a registration that fired apart from an unrelated one that
    // happens to reuse the same (ident, filter) after a concurrent re-add.
    token: u64,
}

// Only ever touched behind KqueueFile::entries' Mutex.
unsafe impl Send for KqueueEntry {}

pub struct KqueueFile {
    entries: Mutex<Vec<KqueueEntry>>,
    next_token: AtomicU64,
    // Bumped by every NOTE_TRIGGER so that a kevent() blocked on this kqueue wakes up. Its address
    // is stable for the life of the descriptor (KqueueFile lives in an Arc in the fd table), so a
    // waiter can sleep on it across the fd-slot lock being dropped.
    user_wake: AtomicU64,
}

impl KqueueFile {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            next_token: AtomicU64::new(0),
            user_wake: AtomicU64::new(0),
        }
    }

    /// Apply a changelist to the persistent registration set, pushing an EV_ERROR receipt for any
    /// entry that couldn't be applied, plus a success receipt (data == 0) for any entry that asked
    /// for EV_RECEIPT. Returns true if a NOTE_TRIGGER landed and waiters need waking.
    fn apply_changes(&self, changelist: &[kevent], out: &mut Vec<kevent>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let slots = get_fd_slots().lock().unwrap();
        let mut triggered = false;
        for change in changelist {
            let is_user = change.filter == EVFILT_USER;
            let res: core::result::Result<(), i32> = 'apply: {
                if !is_user && filter_to_wait_kind(change.filter).is_none() {
                    break 'apply Err(libc::EINVAL);
                }
                let pos = entries
                    .iter()
                    .position(|e| e.ident == change.ident && e.filter == change.filter);

                if change.flags & EV_DELETE != 0 {
                    break 'apply match pos {
                        Some(pos) => {
                            entries.remove(pos);
                            Ok(())
                        }
                        None => Err(libc::ENOENT),
                    };
                }

                if change.flags & EV_ADD != 0 {
                    // EVFILT_USER's ident is not a descriptor, so there is nothing to validate.
                    if !is_user && slots.get(change.ident).is_none() {
                        break 'apply Err(libc::EBADF);
                    }
                    let entry = KqueueEntry {
                        ident: change.ident,
                        filter: change.filter,
                        enabled: change.flags & EV_DISABLE == 0,
                        oneshot: change.flags & EV_ONESHOT != 0,
                        fflags: change.fflags,
                        udata: change.udata,
                        // Re-adding an existing EVFILT_USER registration must not drop a trigger
                        // that hasn't been reported yet: mio's Waker::wake() is exactly an EV_ADD
                        // carrying NOTE_TRIGGER, and two wakes before a poll must not cancel out.
                        triggered: pos.map_or(false, |pos| entries[pos].triggered),
                        token: self.next_token.fetch_add(1, Ordering::Relaxed),
                    };
                    match pos {
                        Some(pos) => entries[pos] = entry,
                        None => entries.push(entry),
                    }
                } else {
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
                        None => break 'apply Err(libc::ENOENT),
                    }
                }

                if is_user && change.fflags & NOTE_TRIGGER != 0 {
                    if let Some(pos) = entries
                        .iter()
                        .position(|e| e.ident == change.ident && e.filter == change.filter)
                    {
                        entries[pos].triggered = true;
                        triggered = true;
                    }
                }
                Ok(())
            };

            match res {
                Err(errno) => out.push(receipt_event(change, errno)),
                Ok(()) if change.flags & EV_RECEIPT != 0 => out.push(receipt_event(change, 0)),
                Ok(()) => {}
            }
        }
        drop(slots);
        drop(entries);

        if triggered {
            self.user_wake.fetch_add(1, Ordering::SeqCst);
            let _ = sys_thread_sync(
                &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                    ThreadSyncReference::Virtual(&self.user_wake),
                    usize::MAX,
                ))],
                None,
            );
        }
        triggered
    }

    /// Consume the pending trigger on each of `fired`, identified the same way EV_ONESHOT
    /// reconciliation identifies entries. EVFILT_USER is always clear-on-report.
    fn clear_triggers(&self, fired: &[(usize, u64)]) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.iter_mut() {
            if e.filter == EVFILT_USER && fired.contains(&(e.ident, e.token)) {
                e.triggered = false;
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
        let mut receipts = Vec::new();
        kqf.apply_changes(changelist, &mut receipts);
        for e in receipts {
            if out_count >= eventlist.len() {
                break;
            }
            eventlist[out_count] = e;
            out_count += 1;
        }

        // Receipts filled the eventlist, so there is no room to report readiness -- return rather
        // than wait. This is what makes an EV_RECEIPT-on-every-change registration call (mio's
        // Selector::register sizes eventlist to nchanges) non-blocking.
        if out_count >= eventlist.len() {
            return Ok(out_count);
        }

        // Sample the trigger counter before snapshotting registrations. apply_changes sets an
        // entry's `triggered` flag and only then bumps this counter, so sampling in this order
        // means a NOTE_TRIGGER racing with us can only make the sleep below return early -- never
        // be missed.
        let user_wake_val = kqf.user_wake.load(Ordering::SeqCst);

        // Snapshot the currently-enabled registrations. We build the wait set from this snapshot
        // (rather than holding `entries` locked across the blocking syscall below), and reconcile
        // EV_ONESHOT removal and EVFILT_USER trigger consumption against it afterward.
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
        // Must be held alive for as long as `wps` may still be read (through the
        // sys_thread_sync call below) -- see WaitpointResult::keepalive.
        let mut keepalives = Vec::new();
        let mut has_user = false;
        for (idx, entry) in snapshot.iter().enumerate() {
            if entry.filter == EVFILT_USER {
                has_user = true;
                if entry.triggered {
                    ready.push(idx);
                }
                continue;
            }
            let Some(wk) = filter_to_wait_kind(entry.filter) else {
                continue;
            };
            let Some(fd) = slots.get(entry.ident) else {
                continue;
            };
            let Ok(wp) = fd.file.waitpoint(wk) else {
                continue;
            };
            if wp.ready || wp.sleep.ready() {
                ready.push(idx);
            } else {
                wps.push(ThreadSync::new_sleep(wp.sleep));
                info.push(idx);
                keepalives.push(wp.keepalive);
            }
        }
        drop(slots);

        if ready.is_empty() {
            // One sleep covers every EVFILT_USER registration on this kqueue; which of them fired
            // is worked out by re-reading the `triggered` flags below. It goes on the end so the
            // `info`-bounded zip that follows skips it.
            if has_user {
                wps.push(ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&kqf.user_wake),
                    user_wake_val,
                    ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                )));
            }
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
            if has_user {
                let entries = kqf.entries.lock().unwrap();
                for (idx, entry) in snapshot.iter().enumerate() {
                    if entry.filter == EVFILT_USER
                        && entries.iter().any(|e| {
                            e.filter == EVFILT_USER
                                && e.ident == entry.ident
                                && e.token == entry.token
                                && e.triggered
                        })
                    {
                        ready.push(idx);
                    }
                }
            }
        }

        let mut oneshot_fired = Vec::new();
        let mut user_fired = Vec::new();
        for idx in ready {
            if out_count >= eventlist.len() {
                break;
            }
            let entry = &snapshot[idx];
            eventlist[out_count] = kevent {
                ident: entry.ident,
                filter: entry.filter,
                flags: 0,
                fflags: entry.fflags,
                data: 0,
                udata: entry.udata,
                ext: [0; 4],
            };
            out_count += 1;
            if entry.oneshot {
                oneshot_fired.push((entry.ident, entry.filter, entry.token));
            }
            if entry.filter == EVFILT_USER {
                user_fired.push((entry.ident, entry.token));
            }
        }

        if !user_fired.is_empty() {
            kqf.clear_triggers(&user_fired);
        }

        if !oneshot_fired.is_empty() {
            kqf.entries
                .lock()
                .unwrap()
                .retain(|e| !oneshot_fired.contains(&(e.ident, e.filter, e.token)));
        }

        Ok(out_count)
    }
}
