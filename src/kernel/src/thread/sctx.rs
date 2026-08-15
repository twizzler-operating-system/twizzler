//! Per-thread cache of everything a security-context switch needs.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use twizzler_abi::object::ObjID;

use crate::{
    memory::context::{Context, UserContext},
    mutex::Mutex,
    security::{SecurityContext, SecurityContextRef},
    spinlock::Spinlock,
};

type SwitchTarget = <Context as UserContext>::SwitchTarget;

/// Contexts kept on the fast path. A thread ping-pongs between its own compartment and the few it
/// calls into, so a handful covers the steady state.
const CACHE_LEN: usize = 4;

/// A/B switch for the refcount work removed from [`SctxCache::switch`] (pagerperf.md). `true`
/// restores the per-switch `active_count` maintenance and the outgoing-context upgrade that went
/// with it, so the two arms are one rebuild apart rather than one revert apart.
///
/// With it `false`, `SctxStats::nr_active` is derived at stats time instead; see
/// [`crate::security::get_sctx_stats`].
const TRACK_ACTIVE_COUNT: bool = false;

struct Entry {
    id: ObjID,
    tls: u64,
    /// Weak on purpose. A cached entry must not keep a security context alive: `SecCtxMgr::drop`
    /// reaps contexts by strong count, and a cache holding strong references would both defeat
    /// that and make the reap depend on field drop order. It is also what makes `target` safe --
    /// see [`SctxCache::switch`].
    ctx: Weak<SecurityContext>,
    target: SwitchTarget,
}

struct Cache {
    entries: [Option<Entry>; CACHE_LEN],
    next_victim: usize,
    /// The active context. It lives here, under the same lock as the entries, so that a switch --
    /// which has to read the outgoing context, save its thread pointer, find the incoming one, and
    /// install it as active -- is a single lock acquisition rather than three.
    ///
    /// `Weak`, like the entries, so that nothing here participates in `SecCtxMgr::drop`'s
    /// strong-count reap. The attached map always holds a strong reference to the active context,
    /// so upgrading it cannot fail in practice; a failure is treated as "unknown" rather than
    /// trusted.
    active_id: ObjID,
    active_ctx: Weak<SecurityContext>,
}

impl Cache {
    fn new(active_id: ObjID, active_ctx: &SecurityContextRef) -> Self {
        Self {
            entries: [const { None }; CACHE_LEN],
            next_victim: 0,
            active_id,
            active_ctx: Arc::downgrade(active_ctx),
        }
    }

    fn index_of(&self, id: ObjID) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| matches!(e, Some(e) if e.id == id))
    }

    /// Update a cached thread pointer, reporting whether there was an entry to update.
    fn set_tls(&mut self, id: ObjID, tls: u64) -> bool {
        match self.index_of(id) {
            Some(i) => {
                self.entries[i].as_mut().unwrap().tls = tls;
                true
            }
            None => false,
        }
    }

    /// Install an entry, returning whatever it displaced.
    fn insert(&mut self, entry: Entry) -> Option<Entry> {
        if let Some(i) = self.index_of(entry.id) {
            return self.entries[i].replace(entry);
        }
        if let Some(i) = self.entries.iter().position(|e| e.is_none()) {
            self.entries[i] = Some(entry);
            return None;
        }
        let victim = self.next_victim;
        self.next_victim = (victim + 1) % CACHE_LEN;
        self.entries[victim].replace(entry)
    }
}

/// What a cache hit yields: enough to complete a context switch without touching a map.
pub struct Hit {
    pub tls: u64,
    pub ctx: SecurityContextRef,
    pub target: SwitchTarget,
}

/// Per-thread cache of the state a security-context switch needs: the thread pointer this thread
/// last used in that context, the context itself, and the page-table root to load.
///
/// A cross-compartment gate entry is a single `SetActiveSctxId` syscall, and this sits on it. In
/// the steady state -- both the outgoing and incoming contexts seen before -- a switch is one
/// spinlock acquisition and a scan of [`CACHE_LEN`] entries, with no map lookup, no sleeping
/// mutex, and no allocation. Every lock in this kernel carries lock-tracking overhead, so the
/// count of them is what the fast path is really minimizing.
///
/// The thread pointer is the one piece that lives nowhere else, so evictions spill it to the map
/// behind the mutex; a context and its switch target can always be looked up again.
///
/// Only the owning thread ever reads or writes this, so neither lock is contended; they are here
/// because a [`super::Thread`] is shared through an `Arc`.
pub struct SctxCache {
    cache: Spinlock<Cache>,
    spill: Mutex<BTreeMap<ObjID, u64>>,
}

/// The outcome of asking the cache to switch contexts.
pub enum Switch {
    /// Already active there; nothing was changed.
    NoSwitch,
    /// Everything needed to complete the switch, which the cache has already committed to.
    Hit(Hit),
    /// Not cached. Carries the outgoing context, which the caller needs and would otherwise have
    /// to take the lock again to read.
    Miss { from: ObjID },
}

impl SctxCache {
    pub fn new(active_id: ObjID, active_ctx: &SecurityContextRef) -> Self {
        Self {
            cache: Spinlock::new(Cache::new(active_id, active_ctx)),
            spill: Mutex::new(BTreeMap::new()),
        }
    }

    /// The active context's id.
    pub fn active_id(&self) -> ObjID {
        self.cache.lock().active_id
    }

    /// The active context, if the weak reference still resolves. The attached map holds a strong
    /// reference to it, so `None` means the caller should fall back to that map.
    pub fn active_ctx(&self) -> Option<SecurityContextRef> {
        let ctx = self.cache.lock().active_ctx.upgrade();
        ctx
    }

    /// Switch to `to`, saving the outgoing context's thread pointer.
    ///
    /// On [`Switch::Hit`] the cache has already recorded `to` as active, so the caller must
    /// complete the switch (thread pointer and page tables) rather than bailing out.
    /// The atomics here are on `SecurityContext`'s refcounts, which every thread in a compartment
    /// shares, so each one is a contended cache line rather than a local increment. The switch used
    /// to do eight of them; it now does two. What went, and why it could:
    ///
    /// - `inc_active_count`/`dec_active_count`. That counter is read by exactly one caller,
    ///   [`crate::security::get_sctx_stats`], and by nothing that affects correctness -- notably
    ///   not `SecCtxMgr::drop`'s reap, which counts strong references. It is now derived by asking
    ///   the threads at stats time instead of being maintained by every switch.
    /// - The `Weak::upgrade` of the outgoing context, and the `Arc` drop that followed it. Those
    ///   existed *only* to have something to call `dec_active_count` on, so they left with it.
    ///
    /// What stays is the incoming context's `upgrade` and the matching drop: the cached `target` is
    /// a page-table root that `SecurityContext::drop` unregisters, so an upgrade that succeeds is
    /// the proof the root is still live, and the strong reference is what keeps it live for the
    /// duration of the switch.
    pub fn switch(&self, to: ObjID, cur_tls: u64) -> Switch {
        let result = {
            let mut cache = self.cache.lock();
            let from = cache.active_id;
            if from == to {
                return Switch::NoSwitch;
            }
            // One pass rather than `set_tls` then `index_of`: both walk the same four entries, and
            // `from != to` is established above, so no entry can match both arms.
            let mut to_idx = None;
            for (i, entry) in cache.entries.iter_mut().enumerate() {
                let Some(entry) = entry else { continue };
                if entry.id == from {
                    // Saved even on a miss, so the slow path does not have to repeat it.
                    entry.tls = cur_tls;
                } else if entry.id == to {
                    to_idx = Some(i);
                }
            }
            let hit = to_idx.and_then(|i| {
                let e = cache.entries[i].as_ref().unwrap();
                Some(Hit {
                    tls: e.tls,
                    target: e.target,
                    ctx: e.ctx.upgrade()?,
                })
            });
            match hit {
                Some(hit) => {
                    let old = if TRACK_ACTIVE_COUNT {
                        hit.ctx.inc_active_count();
                        core::mem::replace(&mut cache.active_ctx, Arc::downgrade(&hit.ctx))
                            .upgrade()
                    } else {
                        cache.active_ctx = Arc::downgrade(&hit.ctx);
                        None
                    };
                    cache.active_id = to;
                    (Switch::Hit(hit), old)
                }
                None => (Switch::Miss { from }, None),
            }
        };
        // Outside the lock: dropping the last reference to a context runs `SecurityContext::drop`,
        // which walks every memory context in the system and takes mutexes.
        let (result, dropped) = result;
        if let Some(old) = dropped {
            old.dec_active_count();
        }
        result
    }

    /// Record a context as active without a cache entry for it, for the slow path.
    pub fn set_active(&self, id: ObjID, ctx: &SecurityContextRef) {
        if TRACK_ACTIVE_COUNT {
            ctx.inc_active_count();
        }
        let old = {
            let mut cache = self.cache.lock();
            cache.active_id = id;
            let old = core::mem::replace(&mut cache.active_ctx, Arc::downgrade(ctx));
            TRACK_ACTIVE_COUNT.then(|| old.upgrade()).flatten()
        };
        if let Some(old) = old {
            old.dec_active_count();
        }
    }

    /// Record what a slow-path switch had to look up.
    pub fn insert(&self, id: ObjID, tls: u64, ctx: &SecurityContextRef, target: SwitchTarget) {
        let evicted = self.cache.lock().insert(Entry {
            id,
            tls,
            ctx: Arc::downgrade(ctx),
            target,
        });
        // Outside the spinlock: this allocates, which is not allowed while holding one (see
        // `VirtContext::register_sctx`).
        if let Some(evicted) = evicted {
            self.spill.lock().insert(evicted.id, evicted.tls);
        }
    }

    /// Save a thread pointer for a context that may not be cached.
    pub fn save_tls(&self, id: ObjID, tls: u64) {
        if self.cache.lock().set_tls(id, tls) {
            return;
        }
        self.spill.lock().insert(id, tls);
    }

    /// The thread pointer saved for `id`, zero if this thread has never run there.
    pub fn saved_tls(&self, id: ObjID) -> u64 {
        if let Some(i) = {
            let cache = self.cache.lock();
            cache
                .index_of(id)
                .map(|i| cache.entries[i].as_ref().unwrap().tls)
        } {
            return i;
        }
        self.spill.lock().get(&id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use twizzler_kernel_macros::kernel_test;

    use super::*;
    use crate::arch::context::ArchContextTarget;

    /// Distinct ids over one real context, since only the `Weak` needs to be live.
    fn ctxs(n: usize) -> Vec<SecurityContextRef> {
        (0..n)
            .map(|_| Arc::new(SecurityContext::new(None)))
            .collect()
    }

    fn hit_tls(s: Switch) -> Option<u64> {
        match s {
            Switch::Hit(h) => Some(h.tls),
            _ => None,
        }
    }

    /// The property the gate path depends on: what was saved for a context comes back the next
    /// time the thread runs in it, and a context never run in reads as zero.
    #[kernel_test]
    fn test_sctx_cache_roundtrip() {
        let held = ctxs(2);
        let (a, b) = (ObjID::new(1), ObjID::new(2));
        let cache = SctxCache::new(a, &held[0]);
        assert_eq!(cache.active_id(), a);

        // `b` is not cached yet, so this misses -- but it must still have saved `a`, and must not
        // have moved the active context.
        assert!(matches!(cache.switch(b, 0x1000), Switch::Miss { from } if from == a));
        assert_eq!(cache.active_id(), a);
        assert_eq!(cache.saved_tls(b), 0);

        // What the slow path would then do.
        cache.set_active(b, &held[1]);
        cache.insert(b, 0x2000, &held[1], ArchContextTarget::null());
        cache.insert(a, 0x1000, &held[0], ArchContextTarget::null());
        assert_eq!(cache.active_id(), b);

        assert_eq!(hit_tls(cache.switch(a, 0x2222)), Some(0x1000));
        assert_eq!(cache.active_id(), a);
        // 0x2222 was `b`'s pointer at the moment of the switch above, so it comes back now.
        assert_eq!(hit_tls(cache.switch(b, 0x1111)), Some(0x2222));
        assert_eq!(hit_tls(cache.switch(a, 0x3333)), Some(0x1111));
    }

    /// Switching to the context already active is a no-op, not a hit.
    #[kernel_test]
    fn test_sctx_cache_no_switch() {
        let held = ctxs(1);
        let a = ObjID::new(1);
        let cache = SctxCache::new(a, &held[0]);
        assert!(matches!(cache.switch(a, 0x1000), Switch::NoSwitch));
    }

    /// More contexts than the cache holds: evicted thread pointers must survive in the map.
    #[kernel_test]
    fn test_sctx_cache_spills() {
        let n = CACHE_LEN + 4;
        let held = ctxs(n);
        let cache = SctxCache::new(ObjID::new(0), &held[0]);
        for i in 0..n {
            cache.insert(
                ObjID::new(i as u128 + 1),
                0x100 + i as u64,
                &held[i],
                ArchContextTarget::null(),
            );
        }
        // The early ones have been evicted; their thread pointers must still be readable.
        for i in 0..n {
            assert_eq!(cache.saved_tls(ObjID::new(i as u128 + 1)), 0x100 + i as u64);
        }
    }

    /// A cache entry must not resurrect a context that has gone away, because its cached
    /// page-table root goes away with it.
    #[kernel_test]
    fn test_sctx_cache_weak() {
        let home = ctxs(1);
        let (h, a) = (ObjID::new(9), ObjID::new(1));
        let cache = SctxCache::new(h, &home[0]);
        {
            let held = Arc::new(SecurityContext::new(None));
            cache.insert(a, 0x1000, &held, ArchContextTarget::null());
            assert_eq!(hit_tls(cache.switch(a, 0x5000)), Some(0x1000));
            cache.set_active(h, &home[0]);
        }
        // The context is gone, so its cached page-table root must not be trusted.
        assert!(matches!(cache.switch(a, 0x5000), Switch::Miss { .. }));
        // The thread pointer outlives it: only the context's own state went away.
        assert_eq!(cache.saved_tls(a), 0x1000);
    }
}
