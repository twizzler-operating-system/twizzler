//! Names the holder of a monitor lock, so a wedge in here leaves something in the transcript.
//!
//! A monitor gate call runs on the *caller's* thread, so a compartment thread can be sitting inside
//! the monitor holding its locks at the moment that compartment is torn down -- and `force_exit`
//! gives the victim no chance to drop a guard. A lock left held by a thread that no longer exists
//! shows up only as every compartment blocking on its next monitor call, with no panic and no
//! output: exactly the shape of the `net_test` hang, whose wedged threads all stop between the two
//! klog lines that bracket `CompartmentHandle::lookup`.
//!
//! Acquisition records holder/site/epoch; a watchdog thread reports a holder whose epoch has not
//! moved. Both are armed by `--diag=monitor` (see [`tracking`]); a default boot records nothing.

use std::{
    cell::Cell,
    panic::Location,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use happylock::{
    collection::LockGuard,
    lockable::{Lockable, RawLock, Sharable},
    rwlock::{RwLockReadGuard, RwLockWriteGuard},
    LockCollection, RwLock, ThreadKey,
};
use twizzler_abi::{klog_println, syscall::sys_thread_self_id};
use twizzler_rt_abi::object::ObjID;

const POLL: Duration = Duration::from_secs(2);
/// Consecutive unchanged samples before a holder is called stuck. Compartment loading legitimately
/// holds these locks for tens of milliseconds; nothing legitimately holds them for ten seconds.
const STALL_SAMPLES: u32 = 5;
const MAX_REPORTS: u32 = 8;
const KILLED_RING: usize = 32;
/// Whether holders are tracked: `--diag=monitor` only. A monitor gate call runs on the caller's
/// thread, so tracking sits on every compartment's object map, thread spawn and compartment
/// operation, and each acquisition pays a `fetch_add` on one shared cache line plus a scan of
/// [`SLOTS`] with a contended `compare_exchange` per occupied slot. Off, `current_hold_site`
/// answers `None` and the watchdog is not started.
fn tracking() -> bool {
    crate::diag_enabled()
}

/// One slot per live acquisition. A single global holder slot cannot express this: monitor code
/// nests these guards, and the inner one's release erased the outer one's record on the way out --
/// leaving the watchdog blind for exactly the case it exists to catch.
const SLOTS: usize = 16;

/// Sequence number of the acquisition occupying each slot; 0 means free, and claiming a slot is the
/// CAS that writes it.
static SLOT_SEQ: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static SLOT_LO: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static SLOT_HI: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// `&'static Location` of the acquisition site, as a usize.
static SLOT_SITE: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
/// Source of slot sequence numbers.
static EPOCH: AtomicU64 = AtomicU64::new(1);

/// Threads the monitor has asked the kernel to force-exit. A holder found in here is the
/// died-holding-the-lock case rather than a slow one.
static KILLED_LO: [AtomicU64; KILLED_RING] = [const { AtomicU64::new(0) }; KILLED_RING];
static KILLED_HI: [AtomicU64; KILLED_RING] = [const { AtomicU64::new(0) }; KILLED_RING];
static KILLED_NEXT: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static SELF_ID: Cell<u128> = const { Cell::new(0) };
}

fn self_id() -> u128 {
    SELF_ID.with(|c| {
        let cached = c.get();
        if cached != 0 {
            return cached;
        }
        let id = sys_thread_self_id().raw();
        c.set(id);
        id
    })
}

/// Wrap a freshly-acquired monitor lock guard so the holder is recorded for its lifetime.
#[track_caller]
pub fn watched<G>(inner: G) -> Watched<G> {
    if !tracking() {
        return Watched {
            inner,
            slot: None,
            held: None,
        };
    }
    let id = self_id();
    let seq = EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    let site = Location::caller() as *const Location<'static> as usize;
    for slot in 0..SLOTS {
        if SLOT_SEQ[slot]
            .compare_exchange(0, seq, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            continue;
        }
        SLOT_LO[slot].store(id as u64, Ordering::Relaxed);
        SLOT_HI[slot].store((id >> 64) as u64, Ordering::Relaxed);
        SLOT_SITE[slot].store(site, Ordering::Release);
        return Watched {
            inner,
            slot: Some(slot),
            held: None,
        };
    }
    // Out of slots: untracked rather than displacing someone else's live record.
    Watched {
        inner,
        slot: None,
        held: None,
    }
}

#[must_use = "a dropped guard releases immediately; bind it to a variable"]
pub struct Watched<G> {
    inner: G,
    slot: Option<usize>,
    held: Option<usize>,
}

impl<G> Drop for Watched<G> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            SLOT_SEQ[slot].store(0, Ordering::Release);
        }
        if let Some(held) = self.held {
            HELD.with(|c| {
                let mut all = c.get();
                all[held] = None;
                c.set(all);
            });
        }
    }
}

/// A monitor lock this thread holds, with the raw release that undoes it. Unlike the diag slots
/// this is always recorded (one thread-local write), because it is what [`release_orphans`]
/// needs when the thread dies inside a gate with no unwind to drop the guard.
#[derive(Clone, Copy)]
struct Held {
    lock: *const (),
    release: unsafe fn(*const ()),
    site: &'static Location<'static>,
    slot: Option<usize>,
}

const HELD_MAX: usize = 4;

thread_local! {
    static HELD: Cell<[Option<Held>; HELD_MAX]> = const { Cell::new([None; HELD_MAX]) };
}

fn held_push(h: Held) -> Option<usize> {
    HELD.with(|c| {
        let mut all = c.get();
        let i = all.iter().position(|e| e.is_none())?;
        all[i] = Some(h);
        c.set(all);
        Some(i)
    })
}

unsafe fn unlock_write<L: RawLock>(lock: *const ()) {
    unsafe { (*lock.cast::<L>()).raw_unlock_write() }
}

unsafe fn unlock_read<L: RawLock>(lock: *const ()) {
    unsafe { (*lock.cast::<L>()).raw_unlock_read() }
}

#[track_caller]
fn watched_held<G, L: RawLock>(inner: G, lock: &L, release: unsafe fn(*const ())) -> Watched<G> {
    let mut w = watched(inner);
    w.held = held_push(Held {
        lock: (lock as *const L).cast(),
        release,
        site: Location::caller(),
        slot: w.slot,
    });
    w
}

#[track_caller]
pub fn watched_read<'a, T>(
    lock: &'a RwLock<T>,
    key: ThreadKey,
) -> Watched<RwLockReadGuard<'a, T, parking_lot::RawRwLock>> {
    watched_held(lock.read(key), lock, unlock_read::<RwLock<T>>)
}

#[track_caller]
pub fn watched_write<'a, T>(
    lock: &'a RwLock<T>,
    key: ThreadKey,
) -> Watched<RwLockWriteGuard<'a, T, parking_lot::RawRwLock>> {
    watched_held(lock.write(key), lock, unlock_write::<RwLock<T>>)
}

#[track_caller]
pub fn watched_lock_all<'a, L: Lockable>(
    locks: &'a LockCollection<L>,
    key: ThreadKey,
) -> Watched<LockGuard<L::Guard<'a>>> {
    watched_held(locks.lock(key), locks, unlock_write::<LockCollection<L>>)
}

#[track_caller]
pub fn watched_read_all<'a, L: Sharable>(
    locks: &'a LockCollection<L>,
    key: ThreadKey,
) -> Watched<LockGuard<L::ReadGuard<'a>>> {
    watched_held(locks.read(key), locks, unlock_read::<LockCollection<L>>)
}

/// Force-release every monitor lock this thread recorded, newest first, for a thread that is
/// about to exit inside a gate. The data behind them may be mid-update; the alternative is a lock
/// held forever by a thread that no longer exists. Returns how many were released.
pub fn release_orphans() -> usize {
    let held = HELD.with(|c| c.replace([None; HELD_MAX]));
    let mut n = 0;
    for h in held.iter().rev().flatten() {
        klog_println!(
            "MONLOCK: dying thread releasing monitor lock taken at {}:{}",
            h.site.file(),
            h.site.line()
        );
        unsafe { (h.release)(h.lock) };
        if let Some(slot) = h.slot {
            SLOT_SEQ[slot].store(0, Ordering::Release);
        }
        n += 1;
    }
    n
}

impl<G: std::ops::Deref> std::ops::Deref for Watched<G> {
    type Target = G::Target;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<G: std::ops::DerefMut> std::ops::DerefMut for Watched<G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// The acquisition site of a monitor lock this thread is currently holding, if any.
///
/// Only this thread writes its own records, and a record is live for exactly the span in which the
/// caller is asking, so the read needs no more synchronization than the slot's own atomics. Names
/// the *monitor* code that is holding a lock, which is what a re-entrant arrival from the runtime
/// otherwise cannot report: the gate call's own stack says only that the runtime called in.
pub fn current_hold_site() -> Option<&'static Location<'static>> {
    if !tracking() {
        return None;
    }
    let id = self_id();
    for slot in 0..SLOTS {
        if SLOT_SEQ[slot].load(Ordering::Acquire) == 0 {
            continue;
        }
        let raw = ((SLOT_HI[slot].load(Ordering::Relaxed) as u128) << 64)
            | SLOT_LO[slot].load(Ordering::Relaxed) as u128;
        if raw != id {
            continue;
        }
        let site = SLOT_SITE[slot].load(Ordering::Acquire);
        if site != 0 {
            return Some(unsafe { &*(site as *const Location<'static>) });
        }
    }
    None
}

/// Count, and name the holder of, an unmap that arrived on a thread already inside the monitor.
///
/// Before this the path was a `tracing::warn!` and a `return`, i.e. the mapping was leaked and
/// nothing said so at a level anyone reads. The site is the interesting half: it names which
/// monitor lock holder dropped an `ObjectHandle`, which is the thing to fix.
pub fn note_recursive_unmap() {
    let n = RECURSIVE_UNMAPS.fetch_add(1, Ordering::Relaxed) + 1;
    if n > MAX_REPORTS as u64 {
        return;
    }
    match current_hold_site() {
        Some(loc) => klog_println!(
            "MONLOCK: recursive unmap #{} deferred, holder at {}:{}",
            n,
            loc.file(),
            loc.line()
        ),
        None => klog_println!("MONLOCK: recursive unmap #{} deferred, holder unknown", n),
    }
}

/// Total recursive unmaps seen, including those past the report cap.
pub fn recursive_unmaps() -> u64 {
    RECURSIVE_UNMAPS.load(Ordering::Relaxed)
}

static RECURSIVE_UNMAPS: AtomicU64 = AtomicU64::new(0);

/// Record that the monitor asked the kernel to force-exit this thread.
pub fn note_killed(id: ObjID) {
    let raw = id.raw();
    let slot = (KILLED_NEXT.fetch_add(1, Ordering::Relaxed) as usize) % KILLED_RING;
    KILLED_LO[slot].store(raw as u64, Ordering::Relaxed);
    KILLED_HI[slot].store((raw >> 64) as u64, Ordering::Relaxed);
}

fn was_killed(raw: u128) -> bool {
    (0..KILLED_RING).any(|i| {
        KILLED_LO[i].load(Ordering::Relaxed) == raw as u64
            && KILLED_HI[i].load(Ordering::Relaxed) == (raw >> 64) as u64
    })
}

pub fn start_watchdog() {
    let _ = std::thread::Builder::new()
        .name("monlockwd".to_string())
        .spawn(|| {
            // Silence from this thread is only evidence if it is known to be running: the first
            // wedge it was meant to catch produced no report and no way to tell why.
            klog_println!("MONLOCK: watchdog running, {} slots", SLOTS);
            let mut last_seq = [0u64; SLOTS];
            let mut same = [0u32; SLOTS];
            let mut reports = 0u32;
            loop {
                std::thread::sleep(POLL);
                for slot in 0..SLOTS {
                    let seq = SLOT_SEQ[slot].load(Ordering::Acquire);
                    if seq == 0 || seq != last_seq[slot] {
                        last_seq[slot] = seq;
                        same[slot] = 0;
                        continue;
                    }
                    same[slot] += 1;
                    if same[slot] < STALL_SAMPLES {
                        continue;
                    }
                    same[slot] = 0;
                    if reports >= MAX_REPORTS {
                        continue;
                    }
                    reports += 1;
                    let raw = ((SLOT_HI[slot].load(Ordering::Relaxed) as u128) << 64)
                        | SLOT_LO[slot].load(Ordering::Relaxed) as u128;
                    let site = SLOT_SITE[slot].load(Ordering::Relaxed);
                    // klog rather than tracing: the wedge this exists for blocks anything that has
                    // to go through the monitor, and a report that needs a monitor lock reports
                    // nothing.
                    if site == 0 {
                        klog_println!(
                            "MONLOCK: held by thread {:x} for >{}s, site unknown, force-exited={}",
                            raw,
                            POLL.as_secs() * STALL_SAMPLES as u64,
                            was_killed(raw)
                        );
                    } else {
                        let loc = unsafe { &*(site as *const Location<'static>) };
                        klog_println!(
                            "MONLOCK: held by thread {:x} for >{}s at {}:{}, force-exited={}",
                            raw,
                            POLL.as_secs() * STALL_SAMPLES as u64,
                            loc.file(),
                            loc.line(),
                            was_killed(raw),
                        );
                    }
                }
            }
        });
}
