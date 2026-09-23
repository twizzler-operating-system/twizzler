//! This mod implements [UserContext] and [KernelMemoryContext] for virtual memory systems.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    marker::PhantomData,
    mem::size_of,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use intrusive_collections::{KeyAdapter, RBTree, RBTreeAtomicLink, intrusive_adapter};
use region::{MapRegion, RegionManager};
use twizzler_abi::{
    device::CacheType,
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID, Protections},
    syscall::{MapControlCmd, MapFlags},
};
use twizzler_rt_abi::error::{ResourceError, TwzError};

use super::{
    KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, UserContext, kernel_context,
};
use crate::{
    arch::{
        address::VirtAddr,
        context::{ArchContext, ArchContextTarget},
    },
    idcounter::{Id, IdCounter, StableId},
    memory::{
        PhysAddr,
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        pagetables::{
            ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings,
            PhysAddrProvider, PhysMapInfo, Table, UninitPageProvider,
        },
        tracker::{FrameAllocFlags, FrameAllocator, take_or_new_frame_allocator},
    },
    mutex::Mutex,
    obj::{
        LookupFlags, ObjectRef, PageNumber, PtGuard, lookup_object, pagetables::ObjectPageTable,
    },
    once::Once,
    processor::{
        mp::current_processor,
        sched::{SchedFlags, schedule},
        spin_wait_until, tls_ready,
    },
    security::KERNEL_SCTX,
    spinlock::Spinlock,
    thread::current_thread_ref,
};

pub mod fault;
pub mod region;
pub mod regionmgr;
mod tests;

pub use fault::page_fault;

/// A type that implements [super::Context] for virtual memory systems.
pub struct VirtContext {
    secctx: Mutex<RBTree<SecctxAdapter>>,
    /// The kernel context's arch state, held outside `secctx` because the kernel has exactly one
    /// security context: there is no map to consult and no lock to take. `Some` here is what makes
    /// this the kernel context.
    ///
    /// Not merely an optimization. Kernel heap growth reaches [`VirtContext::with_arch`] from
    /// inside the allocator's critical section -- ferroc's base allocator calls `allocate_chunk`,
    /// which calls [`GlobalPageAlloc::extend`] -- and `secctx` is a *sleeping* mutex, so taking it
    /// there is the `cannot lock mutex in critical context` panic.
    kernel_arch: Option<ArchContext>,
    // We keep a cache of the actual switch targets so that we don't need to take the above mutex
    // during switch_to. Unfortunately, it's still kinda hairy, since this is a spinlock of a
    // memory-allocating collection. See register_sctx for details.
    target_cache: Spinlock<RBTree<TargetAdapter>>,
    regions: RegionManager,
    id: Id<'static>,
    is_kernel: bool,
}

/// The kernel context's page-table root, cached at boot so that the thread-switch path can reach
/// it without taking any lock. See [`VirtContext::switch_to_kernel_context`].
static KERNEL_ARCH_TARGET: Once<ArchContextTarget> = Once::new();

static CONTEXT_IDS: IdCounter = IdCounter::new();

struct KernelSlotCounter {
    cur_kernel_slot: usize,
    kernel_slots_nums: Vec<Slot>,
}

static KERNEL_SLOT_COUNTER: Once<Mutex<KernelSlotCounter>> = Once::new();

fn kernel_slot_counter() -> &'static Mutex<KernelSlotCounter> {
    KERNEL_SLOT_COUNTER.call_once(|| {
        Mutex::new(KernelSlotCounter {
            cur_kernel_slot: Slot::try_from(VirtAddr::start_kernel_object_memory())
                .unwrap()
                .raw(),
            kernel_slots_nums: Vec::new(),
        })
    })
}

/// A representation of a slot number.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct Slot(usize);

impl Slot {
    fn start_vaddr(&self) -> VirtAddr {
        VirtAddr::new((self.0 * MAX_SIZE) as u64).unwrap()
    }

    fn raw(&self) -> usize {
        self.0
    }

    fn range(&self) -> Range<VirtAddr> {
        let start = self.start_vaddr();
        // The top user slot ends at 0x0000_8000_0000_0000 -- the first address past the lower
        // canonical half -- because SLOTS * MAX_SIZE lands exactly on the canonical hole.
        // `VirtAddr::new` rejects that value, so `offset` returns Err there and the unwrap this
        // replaces panicked the kernel for any range query on the last slot. `end_user_memory()`
        // *is* that address (built directly rather than through `new`), so the range stays exact
        // rather than losing its final byte. The kernel half cannot reach this: object memory
        // stops well short of 2^64.
        let end = start
            .offset(MAX_SIZE)
            .unwrap_or_else(|_| VirtAddr::end_user_memory());
        start..end
    }
}

impl TryFrom<usize> for Slot {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let vaddr = VirtAddr::new((value * MAX_SIZE) as u64).map_err(|_| ())?;
        vaddr.try_into()
    }
}

impl TryFrom<VirtAddr> for Slot {
    type Error = ();

    fn try_from(value: VirtAddr) -> Result<Self, Self::Error> {
        if value.is_kernel() && !value.is_kernel_object_memory() {
            Err(())
        } else {
            Ok(Self(value.raw() as usize / MAX_SIZE))
        }
    }
}

/// Ops resolved per pass by `sys_thread_sync`.
///
/// Sized off the data rather than guessed: multi-op `sys_thread_sync` calls carry ~10
/// virtual-referenced ops on average, so 16 covers essentially all of them in one pass. It also
/// bounds the stack cost of the resolution array, which at the syscall's 1024-op limit would be
/// ~32 KiB on top of the 24 KiB `unsleeps` already there.
pub const RESOLVE_CHUNK: usize = 16;

const MAX_OPP_VEC: usize = 128;
struct ObjectPageProvider {
    pos: usize,
    inner_pos: usize,
    pages: heapless::Vec<(FrameRef, MappingSettings), MAX_OPP_VEC>,
}

impl ObjectPageProvider {
    pub fn new(pages: heapless::Vec<(FrameRef, MappingSettings), MAX_OPP_VEC>) -> Self {
        Self {
            pages,
            pos: 0,
            inner_pos: 0,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages
            .iter()
            .skip(self.pos)
            .fold(0, |acc, x| acc + x.0.nr_pages())
            - self.inner_pos / PageNumber::PAGE_SIZE
    }
}

impl PhysAddrProvider for ObjectPageProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        let page = self.pages.get(self.pos)?;
        if page.0.nr_pages() > 1 {
            log::trace!(
                "peek: {:?}",
                page.0.start_address().offset(self.inner_pos).unwrap()
            );
        }
        Some(PhysMapInfo {
            addr: page.0.start_address().offset(self.inner_pos).unwrap(),
            len: PageNumber::PAGE_SIZE * page.0.nr_pages() - self.inner_pos,
            settings: page.1,
            // Only at the frame's own base: past that the offer is mid-frame, and the frame array
            // is indexed per 4 KiB, so `get_frame(addr)` would resolve to a different `Frame`.
            frame: (self.inner_pos == 0).then_some(page.0),
        })
    }

    fn consume(&mut self, mut len: usize) {
        if len > PageNumber::PAGE_SIZE {
            if len / PageNumber::PAGE_SIZE >= 512 {
                log::trace!("consume: {:?} ({} pages)", len, len / PageNumber::PAGE_SIZE);
            }
        }
        while len > 0 && self.pos < self.pages.len() {
            let rem_len =
                PageNumber::PAGE_SIZE * self.pages[self.pos].0.nr_pages() - self.inner_pos;
            if len < rem_len {
                self.inner_pos += len;
                break;
            } else {
                len = len.saturating_sub(rem_len);
                self.pos += 1;
                self.inner_pos = 0;
            }
        }
    }
}

/// Weak, and that is the fix for the largest leak this kernel has had: these were strong
/// `Arc`s with no removal path anywhere, so every user address space ever created was pinned
/// forever -- and with it its whole `RegionManager` of mappings and every object they
/// referenced. A spawn-storm suite measured ~16k dead compartments' contexts holding 92% of
/// RAM in pending-delete pages (pagerwedge.md §3.8). The entry removes itself in
/// [`VirtContext::drop`].
static ALL_CONTEXTS: Once<Mutex<BTreeMap<u64, Weak<VirtContext>>>> = Once::new();

fn get_all_contexts() -> &'static Mutex<BTreeMap<u64, Weak<VirtContext>>> {
    ALL_CONTEXTS.call_once(|| Mutex::new(BTreeMap::new()))
}

pub fn with_each_context(cb: impl FnMut(&Arc<VirtContext>)) {
    let all = get_all_contexts();
    let contexts = {
        let contexts = all.lock();
        contexts
            .values()
            .filter_map(|w| w.upgrade())
            .collect::<Vec<_>>()
    };
    contexts.iter().for_each(cb);
}

/// One security context's arch state within a [`VirtContext`], linked into two trees at once:
/// `secctx` under a sleeping mutex, and `target_cache` under a spinlock.
///
/// Sharing one allocation between them is the entire point. They used to be two `BTreeMap`s
/// holding separate copies of the same fact, and because filling a map allocates while
/// `target_cache` is a *spinlock*, register/unregister had to rebuild the whole target map off to
/// one side and swap it in. Linking a slot the caller already built costs nothing under either
/// lock, so the rebuild, the swap, and the window between them all go away.
struct SctxSlot {
    secctx_link: RBTreeAtomicLink,
    target_link: RBTreeAtomicLink,
    sctx: ObjID,
    arch: ArchContext,
    /// Callbacks running against `arch` right now, so teardown can wait them out.
    ///
    /// `with_arch` used to hold the `secctx` mutex across its callback, which made it mutually
    /// exclusive with [`VirtContext::unregister_sctx`]. Serving the callback from a snapshot
    /// removes that exclusion, and the `Arc` does not replace it: the `Arc` keeps the *allocation*
    /// alive, but an in-flight callback could still install mappings into an arch context whose
    /// teardown walk had already passed -- leaving them in a root that is then freed. See
    /// `unregister_sctx`, whose own comment explains why a freed root a recycled PCID can still
    /// name is worse than leaked frames.
    ///
    /// Only [`SlotGuard::drop`] ever decrements this. There is deliberately no manual path: the
    /// count is the mechanism the drain waits on, so a leaked decrement would make the drain read
    /// "nobody is using it" *while a callback runs*, which is precisely the bug it exists to
    /// prevent. Structuring the decrement as a guard is what keeps that unrepresentable rather
    /// than merely unlikely.
    users: AtomicUsize,
    /// Set by `unregister_sctx` once the drain has completed and the region walk is about to start
    /// tearing `arch` down.
    ///
    /// This is the independent witness for the drain, and it is deliberately not derived from
    /// `users`: checking `users == 0` after spinning on `users` tests nothing, because the counter
    /// is the mechanism under test. A guard still alive when this is set means the drain let a
    /// callback through, and [`SlotGuard::drop`] catches that -- at drop rather than at use, so it
    /// fires for *any* guard outliving the start of teardown rather than only for one that happens
    /// to touch `arch` at the wrong moment.
    torn_down: AtomicBool,
}

/// A slot borrowed for the duration of one callback. See [`SctxSlot::users`].
struct SlotGuard(Arc<SctxSlot>);

impl SlotGuard {
    fn arch(&self) -> &ArchContext {
        &self.0.arch
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        // Checked *before* the decrement: after it, teardown may proceed and this slot's page
        // tables may be freed, so this is the last moment the observation means anything.
        //
        // `assert!`, not `debug_assert!` -- release builds here set only `debug = true`, so a
        // `debug_assert` would be dead code that reads as coverage. The cost is one acquire load
        // per callback, against the sleeping-mutex acquire/release pair this change removes.
        assert!(
            !self.0.torn_down.load(Ordering::Acquire),
            "sctx slot began teardown while a callback still held it: the users drain let one through"
        );
        // Release, paired with the drain's Acquire load: everything the callback did to the page
        // tables must be visible to the teardown that is waiting on this reaching zero.
        self.0.users.fetch_sub(1, Ordering::Release);
    }
}

intrusive_adapter!(SecctxAdapter = Arc<SctxSlot>: SctxSlot { secctx_link: RBTreeAtomicLink });
impl<'a> KeyAdapter<'a> for SecctxAdapter {
    type Key = ObjID;
    fn get_key(&self, slot: &'a SctxSlot) -> ObjID {
        slot.sctx
    }
}

intrusive_adapter!(TargetAdapter = Arc<SctxSlot>: SctxSlot { target_link: RBTreeAtomicLink });
impl<'a> KeyAdapter<'a> for TargetAdapter {
    type Key = ObjID;
    fn get_key(&self, slot: &'a SctxSlot) -> ObjID {
        slot.sctx
    }
}

impl VirtContext {
    fn __new(kernel_arch: Option<ArchContext>) -> Self {
        let secctx = Mutex::new(RBTree::new(SecctxAdapter::NEW));
        let new = Self {
            regions: RegionManager::default(),
            is_kernel: kernel_arch.is_some(),
            id: CONTEXT_IDS.next(),
            secctx,
            kernel_arch,
            target_cache: Spinlock::new(RBTree::new(TargetAdapter::NEW)),
        };
        new
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Arc<Self> {
        let this = Arc::new(Self::__new(Some(ArchContext::new_kernel())));
        let target = this.arch().target;
        // No `target_cache` entry: the kernel context has exactly one arch context, held in
        // `kernel_arch`, and `single_target` answers for it without touching the tree. That is the
        // same shape `single_arch` already uses, and it makes the kernel's switch lock-free.
        // Cache the root now, while we're safely outside the thread-switch path.
        KERNEL_ARCH_TARGET.call_once(|| target);
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), Arc::downgrade(&this));
        this
    }

    /// Switch the calling processor to the kernel page tables.
    ///
    /// Deliberately lock-free, because the thread-switch path calls this: going through
    /// `switch_to` would take `target_cache`, nesting a spinlock acquisition inside the switch
    /// (which the lock tracker's single intent slot cannot represent, `locktrack.rs:192`) and
    /// serialising every processor that goes idle on one lock. A no-op during very early boot,
    /// before the kernel context exists -- there is nothing to switch away from yet.
    pub fn switch_to_kernel_context() {
        let Some(target) = KERNEL_ARCH_TARGET.poll() else {
            return;
        };
        let proc = tls_ready().then(current_processor);
        // Safety: the kernel context's root outlives every thread, and is never freed.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    /// Construct a new context for userspace.
    pub fn new() -> Arc<Self> {
        let this = Arc::new(Self::__new(None));
        // TODO: remove this once we have full support for user security contexts
        this.register_sctx(KERNEL_SCTX, ArchContext::new());
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), Arc::downgrade(&this));
        this
    }

    /// The kernel context's one arch context. Panics on a user context; see
    /// [`VirtContext::kernel_arch`].
    fn arch(&self) -> &ArchContext {
        self.kernel_arch
            .as_ref()
            .expect("not the kernel context: its arch contexts live in `secctx`")
    }

    /// This context's arch state for `sctx`, without a lock, if there is only one of them.
    fn single_arch(&self, sctx: ObjID) -> Option<&ArchContext> {
        let arch = self.kernel_arch.as_ref()?;
        // Any other sctx on the kernel context missed the (empty) map before this existed, so
        // falling through to it keeps that answer -- `None` from `try_with_arch`, the `expect` from
        // `with_arch` -- rather than handing back the kernel's tables for something else. No caller
        // does this today; the assert is there to say so if one starts.
        debug_assert_eq!(sctx, KERNEL_SCTX);
        (sctx == KERNEL_SCTX).then_some(arch)
    }

    /// This context's switch target for `sctx`, without a lock, if there is only one of them.
    /// Mirrors [`VirtContext::single_arch`], including its fall-through for a non-kernel `sctx`.
    fn single_target(&self, sctx: ObjID) -> Option<ArchContextTarget> {
        let arch = self.kernel_arch.as_ref()?;
        debug_assert_eq!(sctx, KERNEL_SCTX);
        (sctx == KERNEL_SCTX).then_some(arch.target)
    }

    /// The slot for `sctx`, with a use counted against it, taken under the spinlock and returned
    /// with that lock released. The callback then runs holding no lock at all.
    ///
    /// The increment happens *under* the lock, not after it. `unregister_sctx` unlinks under this
    /// same lock and then waits for the count to drain, so an increment landing after the release
    /// could attach to a slot whose drain had already observed zero -- which is the whole race the
    /// count exists to close.
    fn borrow_arch_slot(&self, sctx: ObjID) -> Option<SlotGuard> {
        let slots = self.target_cache.lock();
        let slot = slots.find(&sctx).clone_pointer()?;
        slot.users.fetch_add(1, Ordering::Acquire);
        drop(slots);
        Some(SlotGuard(slot))
    }

    /// Bound on the snapshot [`Self::for_each_arch`] takes. A context has one arch context per
    /// attached security context, which is a handful in practice; overflow falls back to the
    /// mutex path rather than visiting a subset, since a partial walk here is a missed unmap.
    const ARCH_SNAPSHOT: usize = 32;

    /// Run `cb` against every arch context this context owns: the kernel's single one, or a user
    /// context's one per attached security context.
    fn for_each_arch(&self, cb: impl FnMut(&ArchContext)) {
        self.for_each_arch_in(None, cb)
    }

    /// [`Self::for_each_arch`], visiting only arches whose target is in `members` when the set is
    /// known complete. The filter runs *before* the `users` claim: a slot the caller would skip
    /// anyway costs one compare here instead of two RMWs and a guard round trip — measured as
    /// ~11 skipped claims per unmap (20.4M/boot, 94% of visits, `unmap census`). `None` degrades
    /// to visiting everything, exactly the contract the `members` set already carries at its one
    /// cb-side check — which callers keep as a second layer, so the overflow and mutex fallbacks
    /// (which still visit everything) rely on nothing new.
    fn for_each_arch_in(
        &self,
        members: Option<&[ArchContextTarget]>,
        mut cb: impl FnMut(&ArchContext),
    ) {
        if let Some(arch) = self.kernel_arch.as_ref() {
            cb(arch);
            return;
        }
        // Snapshot under the spinlock, iterate outside it: the only caller runs
        // `arch.unmap_object`, which does TLB shootdown and frame frees and cannot run with
        // interrupts masked. Same shape as the `members` set a few hundred lines below.
        let mut snap = heapless::Vec::<SlotGuard, { Self::ARCH_SNAPSHOT }>::new();
        let mut overflow = false;
        {
            let slots = self.target_cache.lock();
            let mut cursor = slots.front();
            while let Some(slot) = cursor.clone_pointer() {
                if let Some(members) = members
                    && !members.contains(&slot.arch.target)
                {
                    // Counted here so the census's skip total keeps meaning "arches the
                    // membership filter excluded", wherever the filter runs.
                    unmap_census::record_skip();
                    cursor.move_next();
                    continue;
                }
                slot.users.fetch_add(1, Ordering::Acquire);
                if snap.push(SlotGuard(slot)).is_err() {
                    overflow = true;
                    break;
                }
                cursor.move_next();
            }
        }
        if !overflow {
            for guard in &snap {
                cb(guard.arch());
            }
            return;
        }
        // More matching contexts than the snapshot holds. Drop what we took and fall through
        // to the mutex path, which visits everything (the cb-side membership check covers it).
        drop(snap);
        for slot in self.secctx.lock().iter() {
            cb(&slot.arch);
        }
    }

    pub fn try_with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> Option<R> {
        if let Some(arch) = self.single_arch(sctx) {
            return Some(cb(arch));
        }
        let guard = self.borrow_arch_slot(sctx)?;
        Some(cb(guard.arch()))
    }

    pub fn with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> R {
        if let Some(arch) = self.single_arch(sctx) {
            return cb(arch);
        }
        let guard = self
            .borrow_arch_slot(sctx)
            .expect("cannot get arch mapper for unattached security context");
        cb(guard.arch())
    }

    /// Page-table frames [`Self::map_object`] can need to map one slot.
    ///
    /// The same count for every slot, which is what lets a caller charge before it has picked one
    /// (`insert_kernel_object` does). A slot is `MAX_SIZE` long and `MAX_SIZE`-aligned, so at each
    /// level it either covers whole tables from offset zero, or -- above `MAX_SIZE` -- sits wholly
    /// inside one entry, however far into it. Neither term depends on which slot.
    /// `test_slot_map_precharge_is_slot_independent` pins that.
    fn slot_map_tables() -> usize {
        MappingCursor::new(VirtAddr::start_user_memory(), MAX_SIZE)
            .max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1)
    }

    /// Charge `fa` with the frames [`Self::slot_map_tables`] counts.
    ///
    /// Callers run this *before* taking the `regions` lock. `WAIT_OK` parks the thread until the
    /// reclaimer frees memory, and `regions` is on the fault path of the whole context
    /// (`fault::get_map_region`), so waiting for memory under it stalls every fault in that context
    /// for the duration. `FrameAllocator::precharge_nowait` names the same rule.
    fn precharge_slot_map(fa: &mut FrameAllocator) {
        fa.set_site(crate::memory::tracker::allocprofile::PC_SITE_MAP);
        fa.precharge(Self::slot_map_tables(), FrameAllocFlags::WAIT_OK);
    }

    pub fn map_object(&self, info: &MapRegion, fa: &mut FrameAllocator) {
        // An explicit target wins; zero means "whatever this thread is running as", which for the
        // monitor is KERNEL_SCTX -- its instance id is zero too. That now resolves (see
        // `security::kernel_sctx`), so those mappings get installed here rather than left to the
        // fault path.
        let sctx = if self.is_kernel {
            // The kernel context has exactly one arch context, registered under KERNEL_SCTX by
            // `new_kernel`, and nothing ever registers another into it. Taking the caller's active
            // sctx here would just make `try_with_arch` miss and silently install nothing --
            // which is what every `insert_kernel_object` from a thread in a real context did.
            KERNEL_SCTX
        } else if info.target_sctx.raw() != 0 {
            info.target_sctx
        } else {
            current_thread_ref()
                .map(|ct| ct.active_sctx_id())
                .unwrap_or(KERNEL_SCTX)
        };

        let len = info.range.end - info.range.start;
        let cursor = MappingCursor::new(info.range.start, len);
        // Reading the thread's own `secctx.active()` instead of `get_sctx(active_id())` is faster
        // (68% of this function). The two used to differ -- `get_sctx(0)` returned `Err` and
        // skipped this whole block -- but both now resolve to the single `kernel_sctx()`, so the
        // swap is available if this shows up in a profile again.
        let sctx = crate::security::get_sctx(sctx);
        // The map count belongs to the *region*, not to whichever arch context happens to install
        // it. Charging the installing arch made the count outlive its creditor: the monitor maps a
        // compartment's stack/comp-config with `target_sctx == 0`, so the charge landed on the
        // calling thread's active sctx -- the compartment's -- while the mapping itself is owned by
        // a monitor `MapHandle` that drops later, asynchronously. When the compartment died its
        // arch went with it and the eventual unmap had nothing to release from: measured as
        // `inc[map=1 fault=0]` on 64/64 stuck objects, each charged to a distinct per-compartment
        // sctx. One region is one count, released when the region is removed, and arch lifetime
        // stops mattering. Taken before any install, so the count is never visible without the
        // mapping (see `insert_object`'s ordering note).
        if info.stable.is_none() {
            let _pt = info.object.lock_page_tables();
            info.object().inc_map_count();
        }
        if let Ok(sctx) = sctx {
            let perms = sctx.lookup(info.object().id(), info.default_prot);
            let mut pt = if info.stable.is_some() {
                PtGuard::new(info.stable.as_ref().unwrap())
            } else {
                info.object.lock_page_tables()
            };
            self.try_with_arch(sctx.id(), |arch| {
                pt.add_invalidate(arch.target, cursor);
                let settings = MappingSettings::new(
                    perms.effective(info.default_prot, info.prot),
                    info.cache_type,
                    MappingFlags::USER,
                );
                let took_ref = arch.object_map(cursor, &mut *pt, settings, fa);
                // Only a region holding the object's *own* tables charges the object. A stable
                // region works on a clone, and the unmap paths mirror this exactly -- their
                // `counted` is `stable.is_none()` -- so charging here would raise a count that
                // nothing ever lowers and leave the object permanently unreapable. Before the
                // count moved onto `Object` this fell out for free: the increment landed on
                // whichever `ObjectPageTable` was in hand, and for a clone that field was never
                // read by anything.
                // No charge here: the region already took one. `took_ref` still governs the
                // arch's own table refcount, it just no longer moves the object's map count.
                let _ = took_ref;
            });
        };
    }

    /// `obj` is the owner of `object_tables`, needed only to charge the map count; the caller
    /// holds its page-table lock as `object_tables`. `None` when `object_tables` is a stable
    /// region's clone rather than the object's own tables -- see the note in [`Self::map_object`]
    /// for why such a mapping takes no count.
    pub fn ensure_object_mapped(
        &self,
        sctxid: ObjID,
        obj: Option<&crate::obj::Object>,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
    ) -> bool {
        // Ask before charging for it. Every fault on an already-resident page reaches here, and
        // only a couple of percent of them install anything, so the frame allocator below was
        // mostly being taken, precharged, and put back to discover there was nothing to do.
        if self.try_with_arch(sctxid, |arch| arch.is_object_mapped(cursor, settings)) == Some(true)
        {
            return false;
        }
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
            FrameAllocFlags::WAIT_OK,
        );
        self.with_arch(sctxid, |arch| {
            object_tables.add_invalidate(arch.target, cursor);
            match arch.ensure_object_mapped(cursor, object_tables, settings, &mut fa) {
                Some(took_ref) => {
                    // A fault installs into an arch but creates no region, so it charges nothing.
                    let _ = (took_ref, obj);
                    true
                }
                None => false,
            }
        })
    }

    /// Every region mapped in this context. Cold path; see [RegionManager::mappings].
    pub fn mappings(&self) -> Vec<Arc<MapRegion>> {
        self.regions.mappings()
    }

    pub fn print_objects(&self) {
        for obj in self.regions.objects() {
            log!("{} => ", obj);
            if let Ok(obj) = lookup_object(obj, LookupFlags::empty()).ok_or(()) {
                for mapping in obj.mappings() {
                    log!("{:?}, ", mapping.range);
                }
            }
            logln!("");
        }
    }

    pub fn register_sctx(&self, sctx: ObjID, arch: ArchContext) {
        if self.kernel_arch.is_some() {
            // The kernel context's one arch context is a field, installed when it was built.
            debug_assert_eq!(sctx, KERNEL_SCTX);
            return;
        }
        // Built before either lock is taken. This is the only allocation the whole operation
        // makes, and moving it here is what removes the rebuild-and-swap: linking the same slot
        // into both trees allocates nothing, so the spinlock no longer bounds what we may do.
        let slot = Arc::new(SctxSlot {
            secctx_link: RBTreeAtomicLink::default(),
            target_link: RBTreeAtomicLink::default(),
            sctx,
            arch,
            users: AtomicUsize::new(0),
            torn_down: AtomicBool::new(false),
        });
        // Slot tree first, `secctx` second -- the reverse of the original order, and it matters
        // now that lookups read the slot tree: inserting there last would leave a window in which
        // a registered sctx is invisible to `with_arch`, which panics on a miss. The duplicate
        // check moves here with it, so one lock decides the race.
        //
        // The flag rather than an early return inside the block: losing the race drops `slot`, and
        // with it an `ArchContext` whose drop frees a root page table. That must not happen under
        // a spinlock.
        let dup = {
            let mut slots = self.target_cache.lock();
            if slots.find(&sctx).is_null() {
                slots.insert(slot.clone());
                false
            } else {
                true
            }
        };
        if dup {
            return;
        }
        self.secctx.lock().insert(slot);
    }

    pub fn unregister_sctx(&self, sctx: ObjID) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Retire the target *before* `arch` is dropped at the end of this function, not after: the
        // drop frees the root page table and releases the PCID, and until the target is gone a
        // concurrent `switch_to` on this sctx would still find it and load it. That keeps a
        // recycled PCID from being installed against a freed root -- which would alias one address
        // space's translations onto whoever gets the PCID next. Unlinking is all this takes now,
        // so the window is the unlink itself rather than a rebuild.
        //
        // Unlinked from the slot tree *first*, before `secctx`: that tree is what
        // `borrow_arch_slot` reads, so removing it there is what stops new callbacks from finding
        // this slot. Doing it
        // in the old order would leave the drain below racing lookups it had already passed.
        let removed = self.target_cache.lock().find_mut(&sctx).remove();

        let slot = {
            let mut secctx = self.secctx.lock();
            let Some(slot) = secctx.find_mut(&sctx).remove() else {
                // No arch state registered here -- the common case at `SecurityContext::drop`
                // time, since the arch slot is usually torn down earlier. Nothing to tear down,
                // and the regions are not this function's to touch: they are released by the
                // monitor dropping its `MapHandle`s, which is refcounted and knows about the
                // other compartments sharing them.
                drop(secctx);
                drop(removed);
                return;
            };
            slot
        };
        drop(removed);

        // Unlinked above, so no new callback can find this slot; wait out the ones already
        // running before the region walk below starts tearing their page tables down. Rare --
        // this runs from `SecurityContext::drop` -- so a spin is the right shape.
        //
        // Cannot deadlock against itself: the only caller is that destructor, reached via
        // `with_each_context`, which iterates outside the ALL_CONTEXTS mutex; and no
        // `with_arch` callback touches a `SecurityContextRef`, so no thread can be
        // inside one while dropping the last reference to the same context. The
        // wait is bounded by callback duration.
        // Yields rather than spinning bare, and that distinction is load-bearing. A
        // `SlotGuard` is held across a callback that does real work -- `arch.object_map`, TLB
        // batching -- so a timer can preempt its holder mid-callback. A pure spin here then
        // never lets that holder run again, which deadlocked the single-vcpu test boot at
        // `st` (schedtest, thread spawn/join churn): 36 of 55 tests, then silence. Measured,
        // not theorised -- the same boot with the old mutex arm ran 55/55.
        //
        // The mutex arm has no such hazard by construction: it *blocks* on `secctx`, and
        // blocking yields the cpu. Replacing a blocking wait with a busy wait is what
        // introduced this, which is the general hazard in the change, not an incidental bug.
        spin_wait_until(
            || (slot.users.load(Ordering::Acquire) == 0).then_some(()),
            || schedule(SchedFlags::YIELD | SchedFlags::PREEMPT | SchedFlags::REINSERT),
        );
        // After the drain, before the walk: from here on any guard still alive is a drain
        // failure, and `SlotGuard::drop` says so. See `SctxSlot::torn_down`.
        slot.torn_down.store(true, Ordering::Release);

        {
            let arch = &slot.arch;
            for region in self.regions.mappings() {
                let cursor = region.mapping_cursor(0, MAX_SIZE);
                // Whichever tree backs this region, as in remove_object: a stable clone still has
                // to be told its mapping is gone, even though it never took a count against the
                // object and so has nothing to give back.
                let mut pt = if let Some(stable) = region.stable.as_ref() {
                    PtGuard::new(stable)
                } else {
                    region.object().lock_page_tables()
                };
                let counted = region.stable.is_none();
                let obj_table = counted.then(|| pt.context_table_addr()).flatten();
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
                pt.remove_invalidate(arch.target, cursor);
                if pt.take_latch_notice() {
                    crate::obj::pagetables::invl_overflow::note_object(
                        region.object().id(),
                        pt.invls_live(),
                        pt.invls_len(),
                    );
                }
                // Tearing an arch down removes no region, so it releases no count. Under the old
                // arch-charged model this walk was the only thing returning those counts, and the
                // ones it could not reach became permanently unreapable.
                let _ = (counted, released);
                let last = false;
                drop(pt);
                if last && region.object().is_pending_delete() {
                    crate::obj::request_reap(region.object());
                }
            }
        }
    }

    /// Init a context for being the kernel context, and clone the mappings from the bootstrap
    /// context.
    pub(super) fn init_kernel_context(&self) {
        let proto = unsafe { Mapper::current() };
        let rm = proto.readmap(MappingCursor::new(
            VirtAddr::start_kernel_memory(),
            usize::MAX,
        ));
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        for map in rm.coalesce() {
            let cursor = MappingCursor::new(map.vaddr(), map.len());
            let settings = MappingSettings::new(
                map.settings().perms(),
                map.settings().cache(),
                map.settings().flags() | MappingFlags::GLOBAL | MappingFlags::WIRED,
            );
            let mut phys = ContiguousProvider::new(map.paddr(), map.len(), settings);
            self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
        }

        // ID-map the lower memory. This is needed by some systems to boot secondary CPUs. This
        // mapping is cleared by the call to prep_smp later.
        //
        // aarch64 needs it to reach the kernel *image*: a secondary comes up in a trampoline that
        // keeps executing at its physical address across the store that enables the MMU, and the
        // bootloader places the image wherever it likes (limine puts it ~13 GB up on virt). x86's
        // trampoline lives in low memory, where 4 GB is plenty.
        #[cfg(target_arch = "aarch64")]
        let id_len = (crate::memory::frame::max_phys_addr() as usize)
            .next_multiple_of(1024 * 1024 * 1024)
            .max(0x100000000);
        #[cfg(not(target_arch = "aarch64"))]
        let id_len = 0x100000000; // 4GB
        let cursor = MappingCursor::new(
            VirtAddr::new(
                Table::level_to_page_size(Table::last_level())
                    .try_into()
                    .unwrap(),
            )
            .unwrap(),
            id_len,
        );
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE | Protections::EXEC,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(
                Table::level_to_page_size(Table::last_level())
                    .try_into()
                    .unwrap(),
            )
            .unwrap(),
            id_len,
            settings,
        );

        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));

        let cursor = MappingCursor::new(VirtAddr::PHYS_START, PhysAddr::phys_mem_map_len());
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE | Protections::EXEC,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(0).unwrap(),
            PhysAddr::phys_mem_map_len(),
            settings,
        );

        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
    }

    /// [`UserContext::lookup_object_ref`] for several slots at once.
    pub fn lookup_object_refs(&self, slots: &[Slot], out: &mut [Option<ObjectRef>]) {
        assert_eq!(slots.len(), out.len());
        for (i, slot) in slots.iter().enumerate() {
            out[i] = self.lookup_object_ref(*slot);
        }
    }

    pub fn lookup_slot(&self, slot: usize) -> Option<Arc<MapRegion>> {
        self.regions.lookup_region(Slot::try_from(slot).ok()?)
    }

    /// Fill `buf` with the numbers of the slots that have something mapped in them, ascending,
    /// skipping the first `offset`. Returns how many were written; short of `buf.len()` means the
    /// enumeration is done. Backs `sys_enumerate_slots`.
    pub fn enumerate_slots(&self, buf: &mut [u64], offset: usize) -> Result<usize, TwzError> {
        let mut slots = self
            .regions
            .mappings()
            .iter()
            .filter_map(|region| {
                Slot::try_from(region.range.start)
                    .ok()
                    .map(|s| s.raw() as u64)
            })
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();

        let count = slots.len().saturating_sub(offset).min(buf.len());
        buf[..count].copy_from_slice(&slots[offset..(offset + count)]);
        Ok(count)
    }
}

impl UserContext for VirtContext {
    type MappingInfo = Slot;
    type SwitchTarget = ArchContextTarget;

    fn switch_target(&self, sctx: ObjID) -> Option<ArchContextTarget> {
        if let Some(target) = self.single_target(sctx) {
            return Some(target);
        }
        self.target_cache
            .lock()
            .find(&sctx)
            .get()
            .map(|slot| slot.arch.target)
    }

    unsafe fn switch_to_target(&self, target: &ArchContextTarget) {
        let proc = tls_ready().then(current_processor);
        // Safety: the caller guarantees the target is still registered here.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    fn switch_to(&self, sctx: ObjID) {
        //let sctx = 0.into();
        if let Some(target) = self.single_target(sctx) {
            let proc = tls_ready().then(current_processor);
            // Safety: the kernel context's root outlives every thread and is never freed.
            unsafe {
                ArchContext::switch_to_target(&target, proc);
            }
            return;
        }
        let tc = self.target_cache.lock();
        let target = &tc
            .find(&sctx)
            .get()
            .expect("tried to switch to a non-registered sctx")
            .arch
            .target;
        // TLS/the processor registry isn't up yet during the very early boot switch from
        // memory::init(); pass None in that case rather than looking up current_processor()
        // from inside the arch-specific switch code.
        let proc = tls_ready().then(current_processor);
        // Safety: we get the target from an ArchContext that we track.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    fn insert_object(
        self: &Arc<Self>,
        slot: Slot,
        object_info: &ObjectContextInfo,
    ) -> Result<(), TwzError> {
        log::debug!(
            "insert {} to {:?} {:?}",
            object_info.object.id(),
            slot.start_vaddr(),
            object_info.prot(),
        );
        let mut stable = None;
        if object_info.flags.contains(MapFlags::STABLE) {
            stable = Some(Arc::new(Mutex::new(
                object_info.object().cow_clone_page_tables()?,
            )));
        }
        let (_is_ok, default_prot) = object_info.object.check_id();
        let new_slot_info = MapRegion {
            prot: object_info.prot(),
            cache_type: object_info.cache(),
            object: object_info.object().clone(),
            offset: 0,
            range: slot.range(),
            flags: object_info.flags,
            target_sctx: object_info.target_sctx(),
            stable,
            default_prot,
            should_sync: AtomicBool::new(false),
            removed: AtomicBool::new(false),
        };

        // Ahead of the lock: see `precharge_slot_map`.
        let mut fa = take_or_new_frame_allocator();
        Self::precharge_slot_map(&mut fa);

        // Claim the slot before mapping, and hold the claim across the map: otherwise a racing
        // insert can clobber our object table entry, and a Busy return leaves behind a mapping
        // plus the map count taken for it, which keeps the object from ever being reaped. The
        // claim is a per-slot state rather than a held lock -- `map_object` takes an object's
        // page-table lock, which is a sleeping mutex. See `SlotState`.
        let guard = self.regions.begin_insert(slot)?;
        // Registered with the object *before* the install takes the map count: `is_reapable`
        // treats "count > 0 with no live mapping" as stale accounting, so the mapping must be
        // visible whenever the count is. The old order (install, then register) left a window
        // where a mid-map object looked stale.
        let region = Arc::new(new_slot_info);
        region.object().add_mapping(slot.raw(), &region);
        self.map_object(&region, &mut fa);
        guard.commit(region);
        Ok(())
    }

    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object(info)
        } else {
            self.regions.lookup_region(info).map(|info| (&*info).into())
        }
    }

    fn lookup_object_ref(&self, info: Self::MappingInfo) -> Option<ObjectRef> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object_ref(info)
        } else {
            self.regions
                .lookup_region(info)
                .map(|region| region.object().clone())
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let Some((slot, guard)) = self.regions.begin_remove(info) else {
            return;
        };
        slot.object().remove_mapping(info.raw());

        // The slot stays claimed for the whole teardown: insert_object claims a free slot and maps
        // immediately (see there), so releasing it here would let another object be mapped into
        // this slot and then have its entry removed by the unmap below. A claim rather than a held
        // lock because the teardown takes sleeping mutexes -- see `SlotState`.
        {
            // Whichever page tables the fault path would use for this region -- taking the same
            // one is what makes the `removed` store below and that path's check of it ordered.
            let mut pt = if let Some(stable) = slot.stable.as_ref() {
                PtGuard::new(stable)
            } else {
                slot.object().lock_page_tables()
            };
            // An in-flight fault now either mapped before us, and the unmap below undoes it, or
            // sees this and does not map at all. See MapRegion::handle_fault.
            slot.removed
                .store(true, core::sync::atomic::Ordering::SeqCst);

            // Stable regions map a private clone of the object's tables, and never took a count
            // against the object (see map_object), so there is nothing to give back for them.
            let counted = slot.stable.is_none();
            let obj_table = counted.then(|| pt.context_table_addr()).flatten();
            let mut n_arches = 0usize;
            let mut n_mapped = 0usize;
            // Stage 3: iterate the contexts that actually hold this object rather than every
            // attached one. The cost being avoided is not the walk -- it is `unmap_object`'s
            // per-context mapper spinlock, taken 45k times a boot to find nothing 94% of the time.
            // So membership filters *before* that call and the walk itself stays.
            //
            // Copied out rather than borrowed because the loop body needs `pt` mutably. `None` here
            // means the set is not known complete, and then this must degrade to exactly the old
            // behaviour -- visiting everything -- which is what makes a wrong membership set a
            // wasted acquisition rather than a missed unmap.
            let members: Option<heapless::Vec<ArchContextTarget, 32>> = (counted)
                .then(|| pt.members().map(|m| m.iter().copied().collect()))
                .flatten();
            self.for_each_arch_in(members.as_deref(), |arch| {
                // Second layer behind for_each_arch_in's pre-filter: this is what the overflow
                // and mutex fallback paths (which visit everything) rely on.
                if let Some(members) = members.as_ref()
                    && !members.contains(&arch.target)
                {
                    unmap_census::record_skip();
                    return;
                }
                let cursor = slot.mapping_cursor(0, MAX_SIZE);
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
                n_arches += 1;
                if released {
                    n_mapped += 1;
                }
                if counted {
                    // Stage 2's validation, and the reason the stage exists: this arch just
                    // released a mapping of this object, so a complete membership set must have
                    // contained it. Checked *before* the removal below, and only where the set
                    // claims to be complete.
                    if released
                        && crate::kdiag_invls()
                        && let Some(members) = pt.members()
                    {
                        crate::obj::pagetables::membership::record_check(
                            members.contains(&arch.target),
                        );
                    }
                    pt.remove_invalidate(arch.target, cursor);
                    if pt.take_latch_notice() {
                        crate::obj::pagetables::invl_overflow::note_object(
                            slot.object().id(),
                            pt.invls_live(),
                            pt.invls_len(),
                        );
                    }
                    // Dec moved out of this loop: it is per-region now, below.
                }
            });
            unmap_census::record(n_arches, n_mapped, counted);
            // The region is going away, so give back the one count it took. Unconditional on what
            // any arch released -- that coupling is what lost counts when an arch died first.
            if counted && slot.object().dec_map_count() == 0 {
                slot.object().note_last_unmap();
            }
            // The map-count leak, caught in the act: a counted removal of a pending-delete
            // object that released nothing anywhere while the count is still positive means the
            // install's arch was neither visited nor already torn down with a dec -- the object
            // is now permanently unreapable (PD-STUCK mapcount, reclaim15). Names the members
            // set and visited count so the skipped arch is identifiable.
            if counted && slot.object().is_pending_delete() {
                let mc = slot.object().map_count();
                // Regions gone (this was the last), count still positive: the object is now
                // permanently unreapable. `n_mapped` says whether this removal released anything
                // (0 = the install's arch was already gone with no dec; >=1 = an arch beyond the
                // visited set still holds an entry).
                if mc > 0 && slot.object().mappings().len() <= 1 {
                    unmap_census::PD_STUCK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    static LEAK_LOGS: core::sync::atomic::AtomicUsize =
                        core::sync::atomic::AtomicUsize::new(0);
                    if LEAK_LOGS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 64 {
                        log::debug!(
                            "unmap leaves stuck mapcount: obj {} mapcount {} released {} visited {} members {:?}",
                            slot.object().id(),
                            mc,
                            n_mapped,
                            n_arches,
                            members
                        );
                    }
                }
            }
            drop(pt);
        }
        guard.finish();

        // After the unmap, not before: syncing can block on the pager, and dirty state lives in the
        // object's own page tables, which unmapping a context's reference to them does not touch.
        if slot.should_sync.load(core::sync::atomic::Ordering::SeqCst) {
            if slot.stable.is_some() {
                // A STABLE mapping writes into its own COW clone of the page tables, so its dirty
                // bits are not the object's and the object-keyed background path cannot see them.
                // Rare (opt-in via MapFlags::STABLE) and worth keeping inline.
                if let Err(e) = slot.ctrl(MapControlCmd::Sync(core::ptr::null_mut()), 0) {
                    log::error!("failed to sync object {}: {:?}", slot.object().id(), e);
                }
            } else {
                // Everything else goes to the background thread. This is what the caller asked
                // for: the sync was registered with SYNC_FLAG_ASYNC_DURABLE and nothing is waiting
                // on it, so the dirty walk and the pager backpressure both belong off this thread.
                crate::pager::queue_background_sync(slot.object());
            }
        }

        // An object marked for deletion while it was still mapped becomes reapable exactly here,
        // and nothing else notices: `ObjectControlCmd::Delete` checks only the object it marks,
        // and the idle loop's whole-map scan does not run at all while a cpu stays busy. Without
        // this, a create/map/delete/unmap loop retains every object it ever made -- measured as
        // `free=0` and memory exhaustion partway through the sysbench suite.
        //
        // Handed to the reaper rather than done here: reaping a pager-backed object issues a
        // delete to the userspace pager, and doing that inline on this path -- with syncs of the
        // same objects in flight -- wedged the contended-sync bench.
        if slot.object().is_pending_delete() {
            crate::obj::request_reap(slot.object());
        }
    }
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtContextSlot {
    obj: ObjectRef,
    slot: Slot,
    prot: Protections,
    cache: CacheType,
    flags: MapFlags,
}

impl From<&VirtContextSlot> for ObjectContextInfo {
    fn from(info: &VirtContextSlot) -> Self {
        ObjectContextInfo::new(info.obj.clone(), info.prot, info.cache, info.flags)
    }
}

impl Drop for VirtContext {
    fn drop(&mut self) {
        // The registry holds a `Weak`, so this remove is bookkeeping, not lifetime: it keeps
        // dead entries from accumulating in the map. Sleeping-lock-safe by the same argument as
        // the rest of this destructor chain (region drops take object page-table mutexes).
        get_all_contexts().lock().remove(&self.id.value());
        // Settle the map-count accounting before the regions and arch contexts are discarded.
        // Making `ALL_CONTEXTS` weak let dead compartments' contexts actually drop -- but a bare
        // drop frees the arch page tables and the `RegionManager` without ever running
        // `dec_map_count` for the installs those arches held. Every object faulted only inside
        // the dying context kept a phantom count of exactly 1 and became permanently unreapable:
        // PD-STUCK measured ~7.3k objects / 2.2M pages (~8.4GB, one ~4MB heap span per dead
        // compartment) standing across many-reclaim12..17. `unregister_sctx` is the existing
        // teardown that walks each arch against each region, decs on release, hands newly
        // unmapped pending-delete objects to the reaper, and sweeps sctx-targeted regions -- run
        // it for every slot still registered. No concurrency to fear: the refcount is zero, so
        // no thread can be switching into or faulting through this context.
        let ids: Vec<ObjID> = self.secctx.lock().iter().map(|slot| slot.sctx).collect();
        for id in ids {
            self.unregister_sctx(id);
        }
    }
}

pub const HEAP_MAX_LEN: usize = 0x0000001000000000 / 16; //4GB

struct GlobalPageAlloc {
    alloc: linked_list_allocator::Heap,
    end: VirtAddr,
}

impl GlobalPageAlloc {
    fn extend(&mut self, len: usize, mapper: &VirtContext) {
        let cursor = MappingCursor::new(self.end, len);
        // TODO: wait-ok?
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        // Uninit, not zeroed: nothing reads kernel heap memory before writing it. The
        // page-table frames `fa` supplies below are a different matter and stay zeroed.
        let mut phys = UninitPageProvider::new(FrameAllocFlags::KERNEL, settings);
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );

        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &mut fa);
        });
        self.end = self.end.offset(len).unwrap();
        // Safety: the extension is backed by memory that is directly after the previous call to
        // extend.
        unsafe {
            self.alloc.extend(len);
        }
    }

    fn init(&mut self, mapper: &VirtContext) {
        let len = 2 * 1024 * 1024;
        let cursor = MappingCursor::new(self.end, len);
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Uninit, not zeroed: nothing reads kernel heap memory before writing it. The
        // page-table frames `fa` supplies below are a different matter and stay zeroed.
        let mut phys = UninitPageProvider::new(FrameAllocFlags::KERNEL, settings);

        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &mut fa);
        });
        self.end = self.end.offset(len).unwrap();
        // Safety: the initial is backed by memory.
        unsafe {
            self.alloc.init(VirtAddr::HEAP_START.as_mut_ptr(), len);
        }
    }
}

// Safety: the internal heap contains raw pointers, which are not Send. However, the heap is
// globally mapped and static for the lifetime of the kernel.
unsafe impl Send for GlobalPageAlloc {}

static GLOBAL_PAGE_ALLOC: Spinlock<GlobalPageAlloc> = Spinlock::new(GlobalPageAlloc {
    alloc: linked_list_allocator::Heap::empty(),
    end: VirtAddr::HEAP_START,
});

impl KernelMemoryContext for VirtContext {
    fn allocate_chunk(&self, layout: core::alloc::Layout) -> Result<NonNull<u8>, TwzError> {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        let res = glb.alloc.allocate_first_fit(layout);
        match res {
            Err(_) => {
                let size = layout
                    .pad_to_align()
                    .size()
                    .next_multiple_of(Table::level_to_page_size(Table::last_level()))
                    * 2;
                glb.extend(size, self);
                glb.alloc
                    .allocate_first_fit(layout)
                    .map_err(|_| ResourceError::OutOfMemory.into())
            }
            Ok(x) => Ok(x),
        }
    }

    unsafe fn deallocate_chunk(&self, layout: core::alloc::Layout, ptr: NonNull<u8>) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        unsafe {
            glb.alloc.deallocate(ptr, layout);
        }
    }

    fn init_allocator(&self) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        glb.init(self);
    }

    fn prep_smp(&self) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        self.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap(
                MappingCursor::new(
                    VirtAddr::start_user_memory(),
                    VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
                ),
                &mut fa,
            )
        });
    }

    type Handle<T> = KernelObjectVirtHandle<T>;

    fn insert_kernel_object<T>(&self, info: ObjectContextInfo) -> Self::Handle<T> {
        // Ahead of the lock: see `precharge_slot_map`.
        let mut fa = take_or_new_frame_allocator();
        Self::precharge_slot_map(&mut fa);

        let mut kernel_slots_counter = kernel_slot_counter().lock();
        let slot = kernel_slots_counter
            .kernel_slots_nums
            .pop()
            .unwrap_or_else(|| {
                let cur = kernel_slots_counter.cur_kernel_slot;
                kernel_slots_counter.cur_kernel_slot += 1;
                let max = Slot::try_from(
                    VirtAddr::end_kernel_object_memory()
                        .offset(-1isize)
                        .unwrap(),
                )
                .unwrap()
                .raw();
                if cur > max {
                    panic!("out of kernel object slots");
                }
                Slot(cur)
            });
        let (_is_ok, default_prot) = info.object().check_id();
        let new_slot_info = MapRegion {
            object: info.object().clone(),
            range: slot.range(),
            offset: 0,
            prot: info.prot(),
            cache_type: info.cache(),
            flags: info.flags,
            target_sctx: info.target_sctx(),
            stable: None,
            default_prot,
            should_sync: AtomicBool::new(false),
            removed: AtomicBool::new(false),
        };
        // Slots come off a free list that is only pushed to once an unmap has fully finished (see
        // `KernelObjectVirtHandle::drop`), so this cannot collide with a teardown in progress.
        let guard = self
            .regions
            .begin_insert(slot)
            .expect("kernel object slot already occupied");
        // Same order as `insert_object`: registered before the install takes the map count.
        let region = Arc::new(new_slot_info);
        region.object().add_mapping(slot.raw(), &region);
        self.map_object(&region, &mut fa);
        guard.commit(region);
        KernelObjectVirtHandle {
            info,
            slot,
            _pd: PhantomData,
        }
    }
}

pub struct KernelObjectVirtHandle<T> {
    info: ObjectContextInfo,
    slot: Slot,
    _pd: PhantomData<T>,
}

impl<T> KernelObjectVirtHandle<T> {
    pub fn start_addr(&self) -> VirtAddr {
        VirtAddr::new(0)
            .unwrap()
            .offset(self.slot.raw() * MAX_SIZE)
            .unwrap()
    }

    pub fn id(&self) -> ObjID {
        self.info.object().id()
    }

    pub fn object(&self) -> &ObjectRef {
        self.info.object()
    }
}

impl<T> Drop for KernelObjectVirtHandle<T> {
    fn drop(&mut self) {
        let kctx = kernel_context();
        // We don't need to tell the object that it's no longer mapped in the kernel context,
        // since object invalidation always informs the kernel context.
        let removal = kctx.regions.begin_remove(self.slot);
        if let Some((region, _)) = &removal {
            region.object().remove_mapping(self.slot.raw());
        }
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let pt = self.object().lock_page_tables();
        // Under the page tables, as in VirtContext::remove_object: a fault holding a clone of this
        // region must not re-map it behind the unmap below.
        if let Some((region, _)) = &removal {
            region
                .removed
                .store(true, core::sync::atomic::Ordering::SeqCst);
        }
        let obj_table = pt.context_table_addr();
        let released = kctx.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap_object(
                MappingCursor::new(self.start_addr(), MAX_SIZE),
                obj_table,
                &mut fa,
            )
        });
        let last = released && self.object().dec_map_count() == 0;
        drop(pt);
        if last {
            self.object().note_last_unmap();
            // Hand the object to the reaper, as every other last-unmap site does. There is no
            // fallback scan, so an object whose *last* mapping was the kernel's KSO handle
            // (sctx objects, thread reprs) was stranded here: marked pending-delete, map count 0,
            // pages never freed -- and via ties, everything tied to it (a dead compartment's heap
            // spans) stayed undeletable too. Measured as PD-SPLIT "unmapped 7142 objs / 2.21M
            // pages" standing in many-reclaim12.
            if self.object().is_pending_delete() {
                crate::obj::request_reap(self.object());
            }
        }
        // Release the slot *before* publishing it to the free list. `insert_kernel_object` pops
        // from that list and claims the slot immediately, and a slot still marked as being torn
        // down would fail that claim.
        drop(removal);
        kernel_slot_counter()
            .lock()
            .kernel_slots_nums
            .push(self.slot);
    }
}

impl<T> KernelObjectHandle<T> for KernelObjectVirtHandle<T> {
    fn base(&self) -> &T {
        unsafe {
            self.start_addr()
                .offset(NULLPAGE_SIZE)
                .unwrap()
                .as_ptr::<T>()
                .as_ref()
                .unwrap()
        }
    }

    fn base_mut(&mut self) -> &mut T {
        unsafe {
            self.start_addr()
                .offset(NULLPAGE_SIZE)
                .unwrap()
                .as_mut_ptr::<T>()
                .as_mut()
                .unwrap()
        }
    }

    fn lea_raw<R>(&self, iptr: *const R) -> Option<&R> {
        let offset = iptr as usize;
        let size = size_of::<R>();
        if offset >= MAX_SIZE || offset.checked_add(size)? >= MAX_SIZE {
            return None;
        }
        unsafe {
            Some(
                self.start_addr()
                    .offset(offset)
                    .unwrap()
                    .as_ptr::<R>()
                    .as_ref()
                    .unwrap(),
            )
        }
    }

    fn lea_raw_mut<R>(&self, iptr: *mut R) -> Option<&mut R> {
        let offset = iptr as usize;
        let size = size_of::<R>();
        if offset >= MAX_SIZE || offset.checked_add(size)? >= MAX_SIZE {
            return None;
        }
        unsafe {
            Some(
                self.start_addr()
                    .offset(offset)
                    .unwrap()
                    .as_mut_ptr::<R>()
                    .as_mut()
                    .unwrap(),
            )
        }
    }
}

impl StableId for VirtContext {
    fn id(&self) -> &Id<'_> {
        &self.id
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}

pub mod unmap_census {
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Buckets: 0, 1, 2, 3, 4, 5-8, 9-16, 17+.
    const NR: usize = 8;
    static ARCHES: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];
    static MAPPED: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static STABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ARCH_VISITS: AtomicUsize = AtomicUsize::new(0);
    static ARCH_HITS: AtomicUsize = AtomicUsize::new(0);
    /// True maxima, because the buckets cannot give one: the top bucket is `17+` and unbounded, so
    /// an empty 9-16 bucket bounds the answer at `<= 8` rather than reporting it. Sizing a
    /// fixed-capacity membership structure off a bound inferred from an empty bucket is guessing.
    static MAX_ARCHES: AtomicUsize = AtomicUsize::new(0);
    static MAX_MAPPED: AtomicUsize = AtomicUsize::new(0);
    /// Arch visits membership let us skip -- i.e. mapper-lock acquisitions not taken. Counted apart
    /// from `ARCH_VISITS` rather than by differencing two runs, so the win is legible within a
    /// single boot and does not depend on a baseline being comparable.
    static SKIPPED: AtomicUsize = AtomicUsize::new(0);

    pub fn record_skip() {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
    }

    /// Objects left permanently unreapable by a removal: last region gone, `map_count` still
    /// positive because `for_each_arch_in` visited none of the membership set. The `log::warn!`
    /// at the detection site is capped at 8 lines, so that cap is a *cap* and not a count --
    /// this is the count. (Requested by twizzler-d3, whose two runs both saturated the cap.)
    pub static PD_STUCK: AtomicUsize = AtomicUsize::new(0);

    /// Detached an object-table entry whose owner could not be verified (`context_table_addr`
    /// was `None`); the dec was taken anyway. See `ArchContext::unmap_object`.
    static UNVERIFIED: AtomicUsize = AtomicUsize::new(0);
    /// Detached an entry that verifiably belonged to a different object; no dec.
    static FOREIGN: AtomicUsize = AtomicUsize::new(0);

    pub fn record_unverified() {
        UNVERIFIED.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_foreign() {
        FOREIGN.fetch_add(1, Ordering::Relaxed);
    }

    fn bucket(n: usize) -> usize {
        match n {
            0..=4 => n,
            5..=8 => 5,
            9..=16 => 6,
            _ => 7,
        }
    }

    pub fn record(arches: usize, mapped: usize, counted: bool) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if !counted {
            STABLE_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        ARCH_VISITS.fetch_add(arches, Ordering::Relaxed);
        ARCH_HITS.fetch_add(mapped, Ordering::Relaxed);
        ARCHES[bucket(arches)].fetch_add(1, Ordering::Relaxed);
        MAPPED[bucket(mapped)].fetch_add(1, Ordering::Relaxed);
        // Guarded by a relaxed load: `fetch_max` has no native instruction on x86_64 or aarch64 and
        // lowers to a `lock cmpxchg` retry loop, so taking it unconditionally puts two contended
        // RMWs on every removal. The race is benign -- the maxima are monotonic and only read at
        // shutdown, so a lost update is re-attempted by the next caller to exceed it.
        if arches > MAX_ARCHES.load(Ordering::Relaxed) {
            MAX_ARCHES.fetch_max(arches, Ordering::Relaxed);
        }
        if mapped > MAX_MAPPED.load(Ordering::Relaxed) {
            MAX_MAPPED.fetch_max(mapped, Ordering::Relaxed);
        }
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        if calls == 0 {
            emerglogln!("== unmap census: none");
            return;
        }
        let visits = ARCH_VISITS.load(Ordering::Relaxed);
        let hits = ARCH_HITS.load(Ordering::Relaxed);
        let g = |a: &[AtomicUsize; NR]| {
            let mut v = [0usize; NR];
            for (i, x) in v.iter_mut().enumerate() {
                *x = a[i].load(Ordering::Relaxed);
            }
            v
        };
        let (ab, mb) = (g(&ARCHES), g(&MAPPED));
        emerglogln!(
            "== unmap census: {} removals ({} stable), {} arch visits ({}/100 mean), {} held a mapping ({}%), {} skipped, max {} arches, max {} mapped",
            calls,
            STABLE_CALLS.load(Ordering::Relaxed),
            visits,
            visits * 100 / calls,
            hits,
            if visits == 0 { 0 } else { hits * 100 / visits },
            SKIPPED.load(Ordering::Relaxed),
            MAX_ARCHES.load(Ordering::Relaxed),
            MAX_MAPPED.load(Ordering::Relaxed),
        );
        emerglogln!(
            "== unmap census releases: {} unverified (dec taken), {} foreign (dec withheld)",
            UNVERIFIED.load(Ordering::Relaxed),
            FOREIGN.load(Ordering::Relaxed)
        );
        emerglogln!(
            "== unmap census pd-stuck: {} objects left unreapable (log capped at 8)",
            PD_STUCK.load(Ordering::Relaxed)
        );
        emerglogln!(
            "== unmap census arches/removal [0,1,2,3,4,5-8,9-16,17+]: {} {} {} {} {} {} {} {}",
            ab[0],
            ab[1],
            ab[2],
            ab[3],
            ab[4],
            ab[5],
            ab[6],
            ab[7]
        );
        emerglogln!(
            "== unmap census mapped/removal  [0,1,2,3,4,5-8,9-16,17+]: {} {} {} {} {} {} {} {}",
            mb[0],
            mb[1],
            mb[2],
            mb[3],
            mb[4],
            mb[5],
            mb[6],
            mb[7]
        );
    }
}
