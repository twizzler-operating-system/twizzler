use crate::{
    arch::{address::VirtAddr, context::ArchContextTarget},
    memory::pagetables::{MappingCursor, TlbOrigin},
};

/// Cache-line maintenance for page-table writes. The walker here may not snoop the data caches,
/// so a written entry's line is cleaned to the point of coherency before the MMU is expected to
/// see it. One line is batched at a time; a different line flushes the batched one first.
#[derive(Default)]
pub struct ArchCacheLineMgr {
    dirty: Option<u64>,
}

impl ArchCacheLineMgr {
    pub fn add_cache_line(&mut self, line: VirtAddr) {
        let addr = line.raw();
        if self.dirty.is_some_and(|dirty| dirty != addr) {
            self.do_flush();
        }
        self.dirty = Some(addr);
    }

    pub fn flush(&mut self) {
        self.do_flush();
    }

    fn do_flush(&mut self) {
        if let Some(addr) = self.dirty.take() {
            unsafe {
                core::arch::asm!(
                    // Clean by VA to the point of coherency, then order it before the walker's
                    // next access.
                    "dc cvac, {}",
                    "dsb ishst",
                    "isb",
                    in(reg) addr
                );
            }
        }
    }
}

impl Drop for ArchCacheLineMgr {
    fn drop(&mut self) {
        self.do_flush();
    }
}

/// One `tlbi vae1is` operand: the page number, ASID 0. Contexts do not use ASIDs, so a VA
/// invalidation reaches every context at once.
#[derive(Clone, Copy, Default)]
struct TlbInvData(u64);

impl TlbInvData {
    const TLBI_SHIFT: usize = 12;

    fn new(addr: VirtAddr) -> Self {
        TlbInvData(addr.raw() >> Self::TLBI_SHIFT)
    }

    fn offset(&self, by: u64) -> Self {
        TlbInvData(((self.0 << Self::TLBI_SHIFT) + by) >> Self::TLBI_SHIFT)
    }

    fn execute(&self) {
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi vae1is, {}",
                "dsb ish",
                "isb",
                in(reg) self.0
            );
        }
    }
}

#[derive(Clone, Copy)]
struct TlbInvQueue {
    data: [TlbInvData; Self::CAPACITY],
    len: u8,
}

impl TlbInvQueue {
    const CAPACITY: usize = 16;

    fn new() -> Self {
        Self {
            data: [TlbInvData::default(); Self::CAPACITY],
            len: 0,
        }
    }

    /// False when full: the caller falls back to a full invalidation rather than executing
    /// early, because a queue may hold object-relative addresses that are only meaningful once
    /// [`ArchTlbMgr::apply_offset_from_map`] has rebased them.
    fn push(&mut self, data: TlbInvData) -> bool {
        if self.len as usize == Self::CAPACITY {
            return false;
        }
        self.data[self.len as usize] = data;
        self.len += 1;
        true
    }

    fn entries(&self) -> &[TlbInvData] {
        &self.data[..self.len as usize]
    }

    fn entries_mut(&mut self) -> &mut [TlbInvData] {
        &mut self.data[..self.len as usize]
    }

    fn drain(&mut self) {
        for inv in self.entries() {
            inv.execute();
        }
        self.len = 0;
    }
}

/// The invalidations queued by one page-table operation. `tlbi ... is` broadcasts in hardware and
/// completes at the `dsb`, so there is no remote half to wait for: [`Self::finish_send`] runs
/// everything and hands back an empty token.
#[derive(Clone)]
pub struct ArchTlbMgr {
    queue: TlbInvQueue,
    target: ArchContextTarget,
    /// Everything, on every core, via `tlbi vmalle1is` instead of the queue.
    full: bool,
    /// Whether any queued page was global. Reporting only, see the amd64 counterpart.
    global: bool,
}

impl core::fmt::Debug for ArchTlbMgr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArchTlbMgr")
            .field("target", &self.target)
            .field("full", &self.full)
            .field("global", &self.global)
            .field("queued", &self.queue.len)
            .finish()
    }
}

impl ArchTlbMgr {
    pub fn new(target: ArchContextTarget) -> Self {
        Self {
            queue: TlbInvQueue::new(),
            target,
            full: false,
            global: false,
        }
    }

    /// Statistics only on amd64, where a shootdown has a remote half worth attributing.
    pub fn set_origin(&mut self, _origin: TlbOrigin) {}

    pub fn new_full_global() -> Self {
        let mut this = Self::new(ArchContextTarget::null());
        this.set_full_global();
        this
    }

    pub fn set_full_global(&mut self) {
        self.full = true;
        self.global = true;
    }

    pub fn set_full(&mut self) {
        self.full = true;
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    pub fn is_global(&self) -> bool {
        self.global
    }

    pub fn set_target(&mut self, target: ArchContextTarget) {
        self.target = target;
    }

    pub fn reset(&mut self) {
        self.queue.len = 0;
        self.full = false;
        self.global = false;
    }

    /// The same invalidations, rebased from object-relative to `map`'s addresses.
    pub fn apply_offset_from_map(&self, map: &MappingCursor) -> Self {
        let mut this = self.clone();
        let by = map.start().raw();
        for inv in this.queue.entries_mut() {
            *inv = inv.offset(by);
        }
        this
    }

    /// Fold `other`'s invalidations into this one; `other` then has nothing left to run when it
    /// drops. Targets need not match: without ASIDs every VA invalidation is context-wide.
    pub fn merge(&mut self, mut other: Self) {
        self.global |= other.global;
        self.full |= other.full;
        if !self.full {
            for inv in other.queue.entries() {
                if !self.queue.push(*inv) {
                    self.full = true;
                    break;
                }
            }
        }
        other.reset();
    }

    /// Enqueue a new TLB invalidation. `is_global` should be set iff the page is global, and
    /// `is_terminal` iff the invalidation is for a leaf. Both kinds are queued: a table-link
    /// change has to evict the walk-cache entry for that VA as well.
    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, _is_terminal: bool, _level: usize) {
        self.global |= is_global;
        if !self.full && !self.queue.push(TlbInvData::new(addr)) {
            self.full = true;
        }
    }

    pub fn has_pending(&self) -> bool {
        self.full || self.queue.len != 0
    }

    /// Execute all queued invalidations.
    pub fn finish(&mut self) {
        if self.full {
            unsafe {
                core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb");
            }
        } else {
            self.queue.drain();
        }
        self.reset();
    }

    /// Counterpart to the amd64 split: nothing is deferred here, the token is empty.
    pub fn finish_send(&mut self) -> PendingShootdown {
        self.finish();
        PendingShootdown
    }
}

impl Drop for ArchTlbMgr {
    fn drop(&mut self) {
        if self.has_pending() {
            self.finish();
        }
    }
}

/// See the amd64 type this mirrors. Nothing to wait for; it exists so generic code can name one
/// API.
#[must_use]
pub struct PendingShootdown;

impl PendingShootdown {
    pub fn none() -> Self {
        Self
    }

    pub fn wait(self) {}

    pub fn absorb(&mut self, _other: Self) {}
}
