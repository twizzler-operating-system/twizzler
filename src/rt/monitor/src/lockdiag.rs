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
//! moved. Deliberately no clock read on the acquisition path -- that is a syscall, and this runs on
//! every monitor lock.

use std::{
    cell::Cell,
    panic::Location,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use twizzler_abi::{klog_println, syscall::sys_thread_self_id};
use twizzler_rt_abi::object::ObjID;

const POLL: Duration = Duration::from_secs(2);
/// Consecutive unchanged samples before a holder is called stuck. Compartment loading legitimately
/// holds these locks for tens of milliseconds; nothing legitimately holds them for ten seconds.
const STALL_SAMPLES: u32 = 5;
const MAX_REPORTS: u32 = 8;
const KILLED_RING: usize = 32;
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
        };
    }
    // Out of slots: untracked rather than displacing someone else's live record.
    Watched { inner, slot: None }
}

#[must_use = "a dropped guard releases immediately; bind it to a variable"]
pub struct Watched<G> {
    inner: G,
    slot: Option<usize>,
}

impl<G> Drop for Watched<G> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            SLOT_SEQ[slot].store(0, Ordering::Release);
        }
    }
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
