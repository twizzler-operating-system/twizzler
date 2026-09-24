//! Monitor-side reclaim of compartments' cached object handles.
//!
//! A compartment's runtime caches released object handles so a close-then-reopen does not pay for
//! a remap. A cached handle is still a *mapping*, and `obj::scan_deleted` reaps an object only
//! once its map count reaches zero -- so a cached entry keeps a deleted object's pages alive.
//!
//! The runtime bounds that two ways, and both fail for a compartment that goes quiet: its
//! `IDLE_TTL` is checked on release, and its queue bound only fires when something else is
//! released. There is no thread in the reference runtime to sweep from, so expiry hangs off
//! `twz_rt_gc`, and nothing calls that under memory pressure.
//!
//! Measured consequence (`--scenario lowmem` at 1024 MB, see `handlecache-table.md`): 132
//! pending-delete objects, every one still mapped, every pinning region owned by a *live,
//! registered* compartment, 62,857 pages -- ~245 MB, 29% of the machine -- with only twelve
//! compartments alive. Not a teardown backlog and not starvation: the caches simply never swept.
//!
//! This thread is the sweeper the runtime does not have. One of it covers every compartment,
//! including the quiet ones, which is the whole point of moving the decision to the monitor.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread::JoinHandle,
    time::Duration,
};

use happylock::ThreadKey;
use monitor_api::{SLOT_CACHED, SLOT_CLAIMED_MON, SLOT_EMPTY};
use twizzler_rt_abi::object::MapFlags;

use super::{get_monitor, space::MapInfo};

/// How long a published entry may sit before the sweeper takes it.
///
/// Matches the runtime's own `IDLE_TTL`. The runtime still expires on its own whenever it is
/// active; this is the floor for compartments that are not.
const SWEEP_TTL: Duration = Duration::from_secs(2);

/// How often to look. Half the TTL, so an entry is never held much beyond it.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// Idle frames, as a percentage of total, below which the TTL stops applying and every published
/// entry is taken. Matches the kernel tracker's own `Loaded` band boundary, so the monitor starts
/// giving memory back at the point the kernel starts calling itself pressured.
const PRESSURE_IDLE_PCT: usize = 25;

/// Diagnostics that answered the "who holds the pinned mappings" question. Off: they are
/// investigation instruments, not shipping behaviour, and they printed 112 `[spacecensus]` and 200
/// `[compmaps]` lines per round into every boot.
const SWEEP_DIAG: bool = false;

/// Victims taken per pass. Fixed, because this path must not allocate: it runs precisely when
/// allocation is failing, and the monitor allocates from its own compartment object, which needs
/// the kernel to give it pages -- the "freeing memory requires memory" trap. A `Vec` here was
/// exactly that bug. Whatever does not fit is taken on the next pass.
const MAX_VICTIMS: usize = 64;

/// Published / reclaimed / rewarm-lost, cumulative. Printed on change, unconditionally: whether
/// the runtime is publishing *at all* is the first thing a transcript has to be able to answer,
/// and a `tracing::debug!` cannot answer it at the default level.
pub static PUBLISHED: AtomicUsize = AtomicUsize::new(0);
pub static RECLAIMED: AtomicUsize = AtomicUsize::new(0);
/// Claims where the monitor *did* hold a record and handed back a handle to drop.
pub static UNMAPPED: AtomicUsize = AtomicUsize::new(0);
/// Claims where it did not -- i.e. the reclaim was a no-op.
pub static NO_RECORD: AtomicUsize = AtomicUsize::new(0);

/// True when the kernel is short enough that cached handles should go regardless of age.
fn under_pressure() -> bool {
    let stats = twizzler_abi::syscall::sys_memory_stats();
    let t = &stats.tracker;
    if t.total == 0 {
        return false;
    }
    t.idle * 100 / t.total < PRESSURE_IDLE_PCT
}

pub struct HandleSweeper {
    _thread: JoinHandle<()>,
}

impl HandleSweeper {
    pub fn new() -> Self {
        Self {
            _thread: std::thread::Builder::new()
                .name("handle-sweeper".to_string())
                .spawn(|| loop {
                    std::thread::sleep(SWEEP_INTERVAL);
                    sweep_once();
                })
                .unwrap(),
        }
    }
}

/// Milliseconds on the same clock the runtime stamps with.
fn now_ms() -> u64 {
    twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_millis() as u64
}

/// One pass over every compartment's table.
///
/// Collects the victims under the compartment-manager read lock and does the unmapping *after*
/// dropping it: `RunComp::unmap_object` hands back a `MapHandle` whose drop runs the deferred
/// unmap, and holding a monitor lock across that is how the recursive-unmap path in
/// `Monitor::unmap_object` gets entered.
fn sweep_once() {
    let now = now_ms();
    let urgent = under_pressure();
    // Pressure-only. The TTL pass was meant to bound staleness for a compartment that has gone
    // quiet, on the reasoning that a cached handle keeps a deleted object's pages alive. It was
    // firing on memory-rich boots with `urgent=false`, unmapping 64 live-but-cached handles at a
    // time on a machine with no memory problem at all -- which is not what a *reclaimer* is for,
    // whatever it costs.
    //
    // Cost attribution, recorded because the first version of this comment got it wrong: a net
    // throughput regression (twizzler-0a, many-sysb0911fix vs 0910) appeared exactly alongside
    // this sweep's log lines, and was attributed to it. The rerun with this fix in place
    // (many-sysb0911fix2, 5/5) showed the sweep silent and **the regression still present**
    // (+158% pipelined). So the correlation was real and the cause was not: both were downstream
    // of a tree that had changed a lot. Do not cite this fix as having recovered net throughput.
    //
    // The reasoning that stands without the number: an idle-time sweep that unmaps hot handles is
    // on the critical path in effect even when it is off it in mechanism, and reclaiming memory
    // nobody needs cannot pay for the refault it forces.
    if !urgent {
        return;
    }
    // Fixed-size, never heap: see MAX_VICTIMS.
    let mut victims: [Option<(twizzler_rt_abi::object::ObjID, MapInfo)>; MAX_VICTIMS] =
        [const { None }; MAX_VICTIMS];
    let mut n = 0usize;
    let mut seen = 0usize;

    {
        let Some(key) = ThreadKey::get() else {
            return;
        };
        let comps = get_monitor().comp_mgr.read(key);
        for rc in comps.compartments() {
            // Slots left CLAIMED_MON by the *previous* pass: their unmap has been issued, so they
            // are free now. Clearing them here rather than inline below is what stops a slot being
            // republished while its unmap is still outstanding, and it self-heals a pass that died
            // in the middle -- the tombstone is cleared next time round either way.
            // Safety: the config object is mapped by this monitor for the life of the
            // compartment, and every field read below is an atomic.
            let table = unsafe { &(*rc.comp_config_ptr()).handle_table };
            for slot in table.slots.iter() {
                let _ = slot.state.compare_exchange(
                    SLOT_CLAIMED_MON,
                    SLOT_EMPTY,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                );
                if slot.state.load(Ordering::Acquire) != SLOT_CACHED {
                    continue;
                }
                seen += 1;
                if n >= MAX_VICTIMS {
                    continue;
                }
                let stamp = slot.stamp_ms.load(Ordering::Relaxed);
                // Under pressure the age does not matter: a cached handle is memory we can have
                // back right now, and the compartment pays at most a remap for it.
                if !urgent && now.saturating_sub(stamp) < SWEEP_TTL.as_millis() as u64 {
                    continue;
                }
                // Claim before reading the identity for real: winning this is what makes the
                // entry ours, and losing it means the runtime rewarmed the handle.
                if slot
                    .state
                    .compare_exchange(
                        SLOT_CACHED,
                        SLOT_CLAIMED_MON,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    continue;
                }
                let id = slot.id();
                let flags = MapFlags::from_bits_truncate(slot.flags.load(Ordering::Relaxed));
                // Left CLAIMED_MON deliberately; see the tombstone note above.
                victims[n] = Some((rc.instance, MapInfo { id, flags }));
                n += 1;
            }
        }
    }

    PUBLISHED.store(seen, Ordering::Relaxed);
    if n == 0 {
        return;
    }

    for (instance, info) in victims.into_iter().flatten() {
        // The table is compartment-writable, so nothing read out of it is trusted. This is the
        // validation: `unmap_object` removes from the monitor's *own* record of what it mapped
        // for this compartment, so an entry naming a mapping this compartment does not hold --
        // including one naming somebody else's -- finds nothing and does nothing.
        let Some(key) = ThreadKey::get() else {
            return;
        };
        let handle = {
            let comps = get_monitor().comp_mgr.read(key);
            comps
                .get(instance)
                .ok()
                .and_then(|rc| rc.unmap_object(info))
        };
        // Counted separately from the claim: `unmap_object` finding nothing is the difference
        // between "reclaimed 134 handles" and "reclaimed 134 handles and freed nothing", and the
        // claim counter alone cannot tell those apart.
        if handle.is_some() {
            UNMAPPED.fetch_add(1, Ordering::Relaxed);
        } else {
            NO_RECORD.fetch_add(1, Ordering::Relaxed);
        }
        // Outside the lock: dropping the handle is what performs the unmap.
        drop(handle);
    }
    // After the drops above, so the counts reflect what survived this pass's reclaim.
    if SWEEP_DIAG {
        if let Ok(space) = get_monitor().space.lock() {
            space.dump_handle_counts();
        }
    }
    // Per-compartment active-handle counts. The sweeper only ever sees *released* handles, so a
    // compartment holding a large number of live ones is invisible to every counter above -- and
    // that is exactly the population pinning the pending-delete pages.
    if SWEEP_DIAG {
        if let Some(key) = ThreadKey::get() {
            let comps = get_monitor().comp_mgr.read(key);
            for rc in comps.compartments() {
                let n = rc.mapped_object_count();
                if n > 0 {
                    // sctx alongside the name: the kernel census attributes pending-delete pages by
                    // security context, and this is the only place the two can be joined.
                    println!("[compmaps] {} sctx={} active={}", rc.name, rc.sctx, n);
                }
            }
        }
    }
    let total = RECLAIMED.fetch_add(n, Ordering::Relaxed) + n;
    println!(
        "[handlesweep] cached={} reclaimed+={} total={} unmapped={} norecord={} urgent={}",
        seen,
        n,
        total,
        UNMAPPED.load(Ordering::Relaxed),
        NO_RECORD.load(Ordering::Relaxed),
        urgent
    );
}
