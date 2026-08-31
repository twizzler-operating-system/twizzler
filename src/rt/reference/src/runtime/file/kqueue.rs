static KEV_SLEEPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEV_SHORT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
        kevent, kevent_filter, wait_kind, EVFILT_READ, EVFILT_USER, EVFILT_WRITE, EV_ADD, EV_CLEAR,
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
    // EV_CLEAR: report only on a not-ready -> ready transition rather than on level.
    clear: bool,
    // EV_CLEAR only: Some(token) means this registration's readiness has already been reported
    // and it must stay silent until the source's falling-edge counter moves past `token`.
    suppressed: Option<u64>,
    // EVFILT_USER only: monotonic count of NOTE_TRIGGERs posted, and the count as of the last
    // one reported. Ready means count > reported. Counters rather than a flag because a trigger
    // landing between "we decided to report" and "we consumed it" must not be swallowed -- and
    // because mio's Waker::wake() is an EV_ADD, so these have to survive a re-add.
    trigger_count: u64,
    trigger_reported: u64,
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
    /// for EV_RECEIPT. Wakes any blocked kevent() itself if a NOTE_TRIGGER landed.
    fn apply_changes(&self, changelist: &[kevent], out: &mut Vec<kevent>) {
        let mut entries = self.entries.lock().unwrap();
        let slots = get_fd_slots().read().unwrap();
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
                        clear: change.flags & EV_CLEAR != 0,
                        // An EV_ADD re-arms: BSD re-evaluates the filter's current level on re-add,
                        // so a still-ready source reports again rather than staying silent.
                        suppressed: None,
                        // Re-adding an existing EVFILT_USER registration must not drop a trigger
                        // that hasn't been reported yet: mio's Waker::wake() is exactly an EV_ADD
                        // carrying NOTE_TRIGGER, and two wakes before a poll must not cancel out.
                        trigger_count: pos.map_or(0, |pos| entries[pos].trigger_count),
                        trigger_reported: pos.map_or(0, |pos| entries[pos].trigger_reported),
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
                        entries[pos].trigger_count += 1;
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
    }

    /// Set or clear EV_CLEAR suppression. `keys` identifies entries the same way EV_ONESHOT
    /// reconciliation does, each paired with the falling-edge token to suppress until.
    fn set_suppression(&self, keys: &[((usize, kevent_filter, u64), Option<u64>)]) {
        if keys.is_empty() {
            return;
        }
        let mut entries = self.entries.lock().unwrap();
        for e in entries.iter_mut() {
            if let Some((_, value)) = keys
                .iter()
                .find(|(k, _)| *k == (e.ident, e.filter, e.token))
            {
                e.suppressed = *value;
            }
        }
    }

    /// Consume the reported triggers. EVFILT_USER is always clear-on-report. Entries are matched
    /// on `ident` alone, deliberately: unlike EV_ONESHOT reconciliation this must survive an
    /// EV_ADD re-add (which mints a new token), because that is exactly what a waker does.
    /// `reported` is the count observed when the event was emitted, so a trigger posted after
    /// that leaves count > reported and is still delivered.
    fn mark_triggers_reported(&self, fired: &[(usize, u64)]) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.iter_mut() {
            if e.filter != EVFILT_USER {
                continue;
            }
            if let Some((_, reported)) = fired.iter().find(|(id, _)| *id == e.ident) {
                e.trigger_reported = e.trigger_reported.max(*reported);
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
        let binding = get_fd_slots().read().unwrap();
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

        // Sample the wake counter before snapshotting registrations. apply_changes bumps an
        // entry's trigger_count first and only then bumps this counter, so sampling in this order
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

        let slots = get_fd_slots().read().unwrap();
        let mut wps = Vec::new();
        // Per sleep in `wps`: which snapshot entry it belongs to, and whether it is a falling-edge
        // sleep (an EV_CLEAR registration waiting out a readiness it already reported) rather than
        // the usual rising-edge one.
        let mut info: Vec<(usize, bool)> = Vec::new();
        let mut ready = Vec::new();
        // EV_CLEAR registrations observed to have gone not-ready, which re-arms them.
        let mut rearmed = Vec::new();
        // Per ready EVFILT_USER entry: the trigger count observed when we decided to report it.
        let mut user_observed: Vec<(usize, u64)> = Vec::new();
        // Per EV_CLEAR entry: the falling-edge token observed for it this call. None means the
        // kind has no falling-edge waitpoint, which is what makes EV_CLEAR degrade to level.
        let mut clear_observed: Vec<(usize, Option<u64>)> = Vec::new();
        // Must be held alive for as long as `wps` may still be read (through the
        // sys_thread_sync call below) -- see WaitpointResult::keepalive.
        let mut keepalives = Vec::new();
        let mut has_user = false;
        for (idx, entry) in snapshot.iter().enumerate() {
            if entry.filter == EVFILT_USER {
                has_user = true;
                if entry.trigger_count > entry.trigger_reported {
                    ready.push(idx);
                    user_observed.push((idx, entry.trigger_count));
                }
                continue;
            }
            let Some(wk) = filter_to_wait_kind(entry.filter) else {
                continue;
            };
            let Some(fd) = slots.get(entry.ident) else {
                continue;
            };
            // Sample the falling-edge waitpoint *before* reading the level, and remember its
            // token for the whole call. Both orderings matter: sampling after the level check
            // would let a fall racing that check be swallowed (we would suppress on the
            // post-fall counter and then wait for a fall that already happened), and reusing
            // one sample means the token we suppress with is the one we actually made the
            // readiness decision against -- the same discipline as `user_observed` below.
            let down = if entry.clear {
                fd.file.waitpoint_not_ready(wk).ok()
            } else {
                None
            };
            let down_token = down.as_ref().map(|d| d.sleep.value);
            if entry.clear {
                clear_observed.push((idx, down_token));
            }

            let Ok(wp) = fd.file.waitpoint(wk) else {
                continue;
            };
            let level = wp.ready || wp.sleep.ready();
            let key = (entry.ident, entry.filter, entry.token);

            if let Some(token) = entry.suppressed {
                // `sleep.ready()` here is "the falling-edge counter has moved past `token`", i.e.
                // the readiness we already reported went away at some point since -- even if it
                // has come back by now and the level says ready again. That is the case a level
                // check alone cannot see, and missing it would leave this registration silent
                // forever with data pending. `down == None` means this kind cannot express a
                // falling edge, so EV_CLEAR degrades to level for it.
                let already_fell = down
                    .as_ref()
                    .is_some_and(|d| d.sleep.value != token || d.sleep.ready());
                if down.is_none() || already_fell {
                    rearmed.push(key);
                } else {
                    // Still the same readiness we already reported. Wait for it to go away rather
                    // than reporting it again -- re-reporting is exactly what makes a
                    // permanently-writable socket spin a level-triggered consumer.
                    let down = down.unwrap();
                    wps.push(ThreadSync::new_sleep(down.sleep));
                    info.push((idx, true));
                    keepalives.push(down.keepalive);
                    continue;
                }
            }

            if level {
                ready.push(idx);
            } else {
                wps.push(ThreadSync::new_sleep(wp.sleep));
                info.push((idx, false));
                keepalives.push(wp.keepalive);
            }
        }
        drop(slots);

        if ready.is_empty() {
            // One sleep covers every EVFILT_USER registration on this kqueue; which of them fired
            // is worked out by re-reading the trigger counters below. It goes on the end so the
            // `info`-bounded zip that follows skips it.
            if has_user {
                wps.push(ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&kqf.user_wake),
                    user_wake_val,
                    ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                )));
            }
            let __t0 = std::time::Instant::now();
            let __res = sys_thread_sync(&mut wps, timeout);
            let __slept = __t0.elapsed();
            // Age of the last read-readiness rising edge at the moment this sleep returned:
            // isolates the kernel wake+schedule hop from kevent's post-wake bookkeeping (UDPRISE
            // measures through to the app's recv). Deltas beyond 50ms are unrelated old rises
            // (slice timeouts, other kqueues) and are skipped, not averaged in.
            {
                use crate::runtime::file::kinds::socket::engine::{READ_RISE_LAST_NS, RISE_CLOCK};
                static SUM: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
                static CNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
                let rise = READ_RISE_LAST_NS.load(Ordering::Relaxed);
                if rise != 0 {
                    let d = (RISE_CLOCK.get().as_nanos() as u64).saturating_sub(rise);
                    if d < 50_000_000 {
                        SUM.fetch_add(d, Ordering::Relaxed);
                        let n = CNT.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_power_of_two() && twizzler_net::diag_enabled("net") {
                            println!(
                                "KQWAKE n={} avg_us={}",
                                n,
                                SUM.load(Ordering::Relaxed) / n / 1000
                            );
                        }
                    }
                }
            }
            // Denominator first: every timed sleep counts, so a silent report means "none were
            // short", never "the probe never ran".
            if timeout.is_some() {
                KEV_SLEEPS.fetch_add(1, Ordering::Relaxed);
            }
            // Count every short return, but only *print* the error ones. An Ok short return is a
            // wake arriving before the timeout -- normal event delivery, and the overwhelming
            // population (4524/4524 KEVSHORT lines in a full sweep log were ret=0). TIMED_OUT
            // returning early is the kernel fault this tripwire hunts, and it has never fired;
            // when it does, the line still carries the sleeps/short counters as denominator.
            if let Some(t) = timeout {
                if __slept * 4 < t {
                    KEV_SHORT.fetch_add(1, Ordering::Relaxed);
                }
                if __slept * 4 < t && __res.is_err() {
                    let __k = match &__res {
                        Ok(n) => *n as i64,
                        Err(e) if *e == TwzError::TIMED_OUT => -1,
                        Err(_) => -2,
                    };
                    // One console write for the whole line: `klog_println!` issues a syscall per
                    // fragment and a dozen compartments splice each other character by character.
                    use core::fmt::Write as _;
                    struct L {
                        b: [u8; 200],
                        n: usize,
                    }
                    impl core::fmt::Write for L {
                        fn write_str(&mut self, s: &str) -> core::fmt::Result {
                            let e = (self.n + s.len()).min(self.b.len());
                            self.b[self.n..e].copy_from_slice(&s.as_bytes()[..e - self.n]);
                            self.n = e;
                            Ok(())
                        }
                    }
                    let mut l = L { b: [0; 200], n: 0 };
                    // ret: >=0 is Ok(n) -- a wake arrived, kernel behaving correctly.
                    //      -1 is TIMED_OUT -- the timeout fired early, a kernel fault.
                    let _ = writeln!(
                        l,
                        "KEVSHORT req_ms={} actual_us={} ret={} nops={} sleeps={} short={}",
                        t.as_millis() as u64,
                        __slept.as_micros() as u64,
                        __k,
                        wps.len(),
                        KEV_SLEEPS.load(Ordering::Relaxed),
                        KEV_SHORT.load(Ordering::Relaxed),
                    );
                    twizzler_abi::syscall::sys_kernel_console_write(
                        twizzler_abi::syscall::KernelConsoleSource::Console,
                        &l.b[..l.n],
                        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
                    );
                }
            }
            // A long wait that genuinely timed out is what a bench stall looks like from inside
            // this compartment (net_*_within retries on a 2s slice). Fire the engine probe here so
            // the stall window carries ring-word samples -- POLLPROBE otherwise prints on
            // power-of-two call counts, which a stalled compartment stops reaching. Diag-gated
            // inside pollprobe; a healthy run's waits resolve in microseconds and never take this.
            if matches!(__res, Err(TwzError::TIMED_OUT)) && timeout.is_some_and(|t| t.as_millis() >= 1000)
            {
                crate::runtime::file::kinds::socket::engine::sample_rings();
                crate::runtime::file::kinds::socket::engine::pollprobe("kevtimeout");
            }
            match __res {
                Ok(_) => {}
                Err(TwzError::TIMED_OUT) => {}
                Err(e) => return Err(e),
            }
            for (wp, (idx, falling)) in wps.iter().zip(info.iter()) {
                if !wp.ready() {
                    continue;
                }
                if *falling {
                    // Went not-ready: re-arm it, but don't report -- the next kevent() will pick
                    // up the rising edge.
                    let entry = &snapshot[*idx];
                    rearmed.push((entry.ident, entry.filter, entry.token));
                } else {
                    ready.push(*idx);
                }
            }
            if has_user {
                let entries = kqf.entries.lock().unwrap();
                for (idx, entry) in snapshot.iter().enumerate() {
                    if entry.filter != EVFILT_USER {
                        continue;
                    }
                    // Matched on ident alone: a waker's EV_ADD re-add mints a new token, so
                    // matching the snapshot's token here would never find the live entry.
                    let Some(live) = entries
                        .iter()
                        .find(|e| e.filter == EVFILT_USER && e.ident == entry.ident)
                    else {
                        continue;
                    };
                    if live.trigger_count > live.trigger_reported {
                        ready.push(idx);
                        user_observed.push((idx, live.trigger_count));
                    }
                }
            }
        }

        let mut oneshot_fired = Vec::new();
        let mut user_fired = Vec::new();
        let mut suppress = Vec::new();
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
                if let Some((_, observed)) = user_observed.iter().find(|(i, _)| *i == idx) {
                    user_fired.push((entry.ident, *observed));
                }
            } else if entry.clear {
                // Suppress until the source's counter moves past the token sampled when this
                // call decided the entry was ready -- not one re-read now, which would discard a
                // fall that happened in between.
                let token = clear_observed
                    .iter()
                    .find(|(i, _)| *i == idx)
                    .and_then(|(_, t)| *t);
                suppress.push(((entry.ident, entry.filter, entry.token), token));
            }
        }

        // Order matters: an entry re-armed above may also have been reported in the same call (it
        // went not-ready and back before we got here), and must end up suppressed.
        let rearmed: Vec<_> = rearmed.into_iter().map(|k| (k, None)).collect();
        kqf.set_suppression(&rearmed);
        kqf.set_suppression(&suppress);

        if !user_fired.is_empty() {
            kqf.mark_triggers_reported(&user_fired);
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
