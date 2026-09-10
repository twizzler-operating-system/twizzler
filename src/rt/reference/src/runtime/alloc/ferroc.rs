use std::{
    alloc::{AllocError, GlobalAlloc},
    ptr::NonNull,
};

use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{sys_object_copy, ObjectSource},
};

use super::talc::{LocalAllocator, LOCAL_ALLOCATOR};
use crate::{runtime::RuntimeState, OUR_RUNTIME};

/// Hand the frames under `[ptr, ptr + len)` back to the kernel.
///
/// `sys_object_copy`'s zeroing source unmaps every whole page the range covers, so those frames are
/// freed and the next touch faults a fresh zero page. That is the object-system equivalent of the
/// `MADV_DONTNEED` ferroc's own mmap base uses for `decommit`, and it is the only way a page that
/// faulted into a long-lived heap object is ever given back -- without it the heap only ever grows
/// to its high-water mark.
///
/// Best-effort, like `madvise`: a range whose object cannot be named, or one the kernel rejects,
/// simply keeps its memory. Nothing depends on the range reading zero afterwards -- see
/// `IS_ZEROED`.
/// DIAG: `[decommit hook, deallocate hook, ranges entered, id lookup failed, bytes declined]`.
///
/// `sys_object_copy` reads 0 calls for a whole boot even across `l2d`, which allocates, touches and
/// frees 2 MiB forty times. Reading the code, that should retire a huge slab per iteration and
/// reach one of ferroc's two base hooks each time. A zero can mean the hooks are never called or
/// that they are called and this function declines -- `get_id_from_ptr` returning `None` is a
/// silent early return by design -- and those have fixes in different files. Counting both ends
/// separates them.
///
/// Entries 5-7 are the *other* end of the same question: how much memory ferroc has taken from
/// talc as base chunks, and how much it has given back. `hook_dealloc` reading zero says chunks are
/// never returned; `base_alloc_bytes` says how much that is worth. Growth in a `note=heap` object
/// with these flat is talc reusing an address range whose pages were already faulted in -- a
/// different mechanism from ferroc asking for more.
pub(crate) static DECOMMIT_STATS: [core::sync::atomic::AtomicU64; 8] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 8];

pub(crate) const S_BASE_ALLOC_CNT: usize = 5;
pub(crate) const S_BASE_ALLOC_BYTES: usize = 6;
pub(crate) const S_BASE_DEALLOC_BYTES: usize = 7;

fn bump(i: usize, by: u64) {
    DECOMMIT_STATS[i].fetch_add(by, core::sync::atomic::Ordering::Relaxed);
}

/// Diagnostic readout for [`DECOMMIT_STATS`]; `out` must have room for 8. Not part of the runtime
/// ABI.
#[no_mangle]
pub extern "C-unwind" fn __twz_rt_diag_decommit_stats(out: *mut u64) {
    for (i, c) in DECOMMIT_STATS.iter().enumerate() {
        unsafe { *out.add(i) = c.load(core::sync::atomic::Ordering::Relaxed) };
    }
}

/// Zero `len` bytes at `ptr` by asking the kernel to remap the range as zero, rather than storing
/// the bytes.
///
/// Returns false if the range could not be resolved to a heap object, in which case the caller
/// still owes the zeroing -- unlike `decommit_range`, where doing nothing is a valid outcome.
///
/// `ptr` must be page-aligned and `len` a whole number of pages, both because `ObjectSource`
/// works in pages and because a partial page would silently zero a neighbour's bytes.
pub(crate) unsafe fn zero_range(ptr: *mut u8, len: usize) -> bool {
    let Some(id) = id_from_ptr(ptr) else {
        return false;
    };
    let zero = ObjectSource::new_zero((ptr as usize % MAX_SIZE) as u64, len);
    // End-to-end, to compare against the kernel's own `zero_range` breakdown: the difference is
    // everything `sys_object_copy` does around the page-table work.
    let t = std::time::Instant::now();
    let ok = sys_object_copy(id, &[zero.into()]).is_ok();
    ZR_NS.fetch_add(
        t.elapsed().as_nanos() as u64,
        core::sync::atomic::Ordering::Relaxed,
    );
    ZR_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    ok
}

pub static ZR_NS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static ZR_CALLS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

unsafe fn decommit_range(ptr: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    bump(2, 1);
    if let Some(id) = id_from_ptr(ptr) {
        let zero = ObjectSource::new_zero((ptr as usize % MAX_SIZE) as u64, len);
        let _ = sys_object_copy(id, &[zero.into()]);
    } else {
        bump(3, 1);
        bump(4, len as u64);
    }
}

/// ZERO_MEMORY request routing in `twz_rt_malloc`: `[gate calls, gate bytes, arena-decline
/// fallback calls, fallback bytes, sub-gate calls, sub-gate bytes]`. Gate = tried the anon
/// arena; fallback = arena declined and the request landed in the heap; sub-gate = below
/// `LAZY_ZERO_MIN` (or oddly shaped), never offered to the arena. Only counted while
/// [`decommitstats::REPORT_ON`].
pub(crate) static ZSTAT: [core::sync::atomic::AtomicU64; 6] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 6];

#[inline(always)]
pub(crate) fn zbump(i: usize, by: u64) {
    if decommitstats::REPORT_ON {
        ZSTAT[i].fetch_add(by, core::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) mod decommitstats {
    /// Off by default: prints on every compartment exit, and a build exits ~25 of them. Also
    /// arms the heap census at pre_main_hook so MALLOCSTAT totals cover the whole program.
    pub const REPORT_ON: bool = false;

    pub fn report() {
        if !REPORT_ON {
            return;
        }
        use core::sync::atomic::Ordering::Relaxed;
        let d: Vec<u64> = super::DECOMMIT_STATS
            .iter()
            .map(|c| c.load(Relaxed))
            .collect();
        if d[super::S_BASE_ALLOC_CNT] == 0 {
            return;
        }
        // Identity from the monitor (captured at pre-main), not argv: `env::args()` is empty on
        // this system, so this line used to be anonymous -- which meant picking "the biggest by
        // MiB" and hoping it was the same compartment across runs. It was not, and that manifested
        // as 43% run-to-run variance in allocated bytes at a constant allocation count.
        let name = crate::runtime::alloc::sites::ident_string();
        let sctx = secgate::get_sctx_id().raw() as u64;
        let name = format!("{name}/{sctx:016x}");
        let (ac, ab, fc, fb) = crate::runtime::alloc::census::totals();
        secgate::statcadence::report_forced(format_args!(
            "MALLOCSTAT {name} allocs={ac}/{}MiB frees={fc}/{}MiB peaklive={}MiB livenow={}MiB \
             implied={}MiB underflow={}",
            ab >> 20,
            fb >> 20,
            crate::runtime::alloc::census::peak_live() >> 20,
            crate::runtime::alloc::census::live_now() >> 20,
            (ab.saturating_sub(fb)) >> 20,
            crate::runtime::alloc::census::UNDERFLOW.load(core::sync::atomic::Ordering::Relaxed),
        ));
        // Which size classes the heap is still holding at exit -- the shape that separates
        // "a few thousand large buffers retained" from "diffuse growth everywhere".
        let ret = crate::runtime::alloc::census::retained_by_class();
        if !ret.is_empty() {
            let mut buf = String::with_capacity(160);
            for (c, n, b) in ret.iter().take(6) {
                use core::fmt::Write as _;
                let _ = write!(buf, " c{}({}B)={}blk/{}MiB", c, 1u64 << c, n, b >> 20);
            }
            secgate::statcadence::report_forced(format_args!("RETAIN {name}{buf}"));
        }
        let z: [u64; 6] = core::array::from_fn(|i| super::ZSTAT[i].load(Relaxed));
        let a: [u64; 8] =
            core::array::from_fn(|i| crate::runtime::alloc::anon::DECLINE[i].load(Relaxed));
        if z[0] + z[4] > 0 {
            secgate::statcadence::report_forced(format_args!(
                "ANONSTAT {name} gate={}/{}MiB heap-fallback={}/{}MiB subgate={}/{}MiB | \
                 arena: virgin={} reused={} early={} notls={} full={} create={} zerofail={} newarena={}",
                z[0], z[1] >> 20, z[2], z[3] >> 20, z[4], z[5] >> 20,
                a[0], a[5], a[1], a[2], a[3], a[4], a[6], a[7],
            ));
        }
        secgate::statcadence::report_forced(format_args!(
            "FERROC-BASE {name} base_alloc={}/{}MiB dealloc={}/{}MiB decommit_hook={} ranges={} no_id={}/{}MiB",
            d[super::S_BASE_ALLOC_CNT],
            d[super::S_BASE_ALLOC_BYTES] >> 20,
            d[1],
            d[super::S_BASE_DEALLOC_BYTES] >> 20,
            d[0],
            d[2],
            d[3],
            d[4] >> 20,
        ));
    }
}

/// Give ferroc object-backed base chunks directly, instead of sub-allocating them out of talc
/// spans. talc writes an in-band free-list header into every span it carves, faulting pages
/// ferroc's own out-of-band slab bitmap never touches; a fresh object is provably zero and needs
/// no such metadata. `false` restores the talc base — this is the A/B. Monitor keeps talc either
/// way (its objects are mapped directly, not delete-on-create).
pub(crate) const USE_OBJECT_BASE: bool = true;

/// Skip the object's null + metadata pages and keep the chunk `SLAB_SIZE`-aligned; reserve the
/// tail so a chunk never runs into the metadata/FOT region at the object's top.
const OBJ_BASE_SKIP: usize = ferroc::config::SLAB_SIZE;
const OBJ_BASE_USABLE: usize = MAX_SIZE - 2 * ferroc::config::SLAB_SIZE;

/// One object per base chunk handed to ferroc, tracked out-of-band so `id_from_ptr` (decommit /
/// zero_range) can name it and `deallocate` can free it. Fixed capacity + a plain array so the
/// base path never allocates — a `Vec` growth here would re-enter ferroc, hence its own base.
const OBJ_BASE_CAP: usize = 512;
struct ObjBaseTable {
    ents: [(usize, u128); OBJ_BASE_CAP],
    len: usize,
}
static OBJ_BASE: std::sync::Mutex<ObjBaseTable> = std::sync::Mutex::new(ObjBaseTable {
    ents: [(0, 0); OBJ_BASE_CAP],
    len: 0,
});
/// Set once the object base hands out its first chunk. `obj_base_id`'s lock-free fast path reads
/// it to avoid touching `OBJ_BASE`'s mutex while the table is still empty (that path runs on the
/// hot decommit/zero_range path early in a compartment's life, before it is safe to sleep).
static OBJ_BASE_PRIMED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Threads currently inside the object base's `create_and_map` gate call. That gate call can
/// itself trigger an allocation on the same thread (e.g. deep in `cross_compartment_entry`'s
/// thread bring-up), which re-enters `allocate`; a reentrant grow that gate-called again would
/// deadlock. The reentrant grow uses talc instead (its existing span, no gate). Keyed by kernel
/// thread id -- TLS-free, so it is valid even in the zero-thread-pointer window.
static IN_GATE: [core::sync::atomic::AtomicU64; 32] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 32];

struct GateGuard(u64);
impl Drop for GateGuard {
    fn drop(&mut self) {
        use core::sync::atomic::Ordering::*;
        for s in IN_GATE.iter() {
            if s.load(Acquire) == self.0 {
                s.store(0, Release);
                return;
            }
        }
    }
}

/// `Some(guard)` if this thread may make the object-base gate call; `None` if it is already inside
/// one (reentrant) or the id is unavailable -- in which case the caller falls back to talc.
fn gate_enter() -> Option<GateGuard> {
    use core::sync::atomic::Ordering::*;
    let tid = twizzler_abi::syscall::sys_thread_self_id().raw() as u64;
    if tid == 0 {
        return None;
    }
    for s in IN_GATE.iter() {
        if s.load(Acquire) == tid {
            return None;
        }
    }
    for s in IN_GATE.iter() {
        if s.compare_exchange(0, tid, AcqRel, Relaxed).is_ok() {
            return Some(GateGuard(tid));
        }
    }
    None
}

/// The object id backing `slot`, if `slot` was handed out by the object base.
fn obj_base_id(slot: usize) -> Option<twizzler_rt_abi::object::ObjID> {
    // Lock-free fast path: the table is empty until the object base hands out its first chunk, and
    // `id_from_ptr` runs on the hot decommit/zero_range path early in a compartment's life -- long
    // before it is safe to sleep on a `std::sync::Mutex`. Never touch the lock while empty.
    if !USE_OBJECT_BASE || !OBJ_BASE_PRIMED.load(core::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let t = OBJ_BASE.lock().unwrap_or_else(|e| e.into_inner());
    t.ents[..t.len]
        .iter()
        .find(|(s, _)| *s == slot)
        .map(|(_, id)| twizzler_rt_abi::object::ObjID::new(*id))
}

/// Remove and return the id for `slot` on free.
fn obj_base_take(slot: usize) -> Option<twizzler_rt_abi::object::ObjID> {
    let mut t = OBJ_BASE.lock().unwrap_or_else(|e| e.into_inner());
    let i = t.ents[..t.len].iter().position(|(s, _)| *s == slot)?;
    let id = twizzler_rt_abi::object::ObjID::new(t.ents[i].1);
    let last = t.len - 1;
    t.ents[i] = t.ents[last];
    t.len = last;
    Some(id)
}

/// Object id for an arbitrary heap pointer: object base first, then talc's span list. Both
/// `zero_range` and `decommit_range` resolve pointers through here so either base is nameable.
fn id_from_ptr(ptr: *const u8) -> Option<twizzler_rt_abi::object::ObjID> {
    let slot = ptr as usize / MAX_SIZE;
    obj_base_id(slot).or_else(|| LOCAL_ALLOCATOR.get_id_from_ptr(ptr))
}

pub struct TwzFerrocBase {
    pub local_alloc: &'static LocalAllocator,
}

impl TwzFerrocBase {
    pub const fn new() -> Self {
        Self {
            local_alloc: &LOCAL_ALLOCATOR,
        }
    }
}

unsafe impl ferroc::base::BaseAlloc for TwzFerrocBase {
    // True: `decommit_range` zeroes every range it is given, so a re-committed range reads zero and
    // `calloc` can skip its memset.
    //
    // This was `false` until 2026-08-18 because of a kernel bug, not anything in ferroc:
    // `Table::setup_zero_range` advanced its walk cursor twice per level-1 region, so any decommit
    // crossing a 2 MiB boundary zeroed only the first region and still returned `Ok(())`. Slabs are
    // 4 MiB *and* slab-aligned, so every slab decommit crossed one and roughly half of what the
    // runtime believed it was returning stayed dirty. Validated with this constant `true`: 6/6 runs
    // and zero `post_alloc` violations, against 0/2 on `debug-kvm-smp1` before (tag `zerofix3`),
    // plus a control that re-introduced only the double-advance and failed at exactly the 2 MiB
    // boundary. See `ferroc.md`.
    //
    // **Any change here needs a debug arm.** ferroc's zero check is a `debug_assert!`
    // (`heap.rs:376`), so a release build cannot tell you this is wrong -- it will quietly hand
    // stale bytes to `calloc` with every test green.
    const IS_ZEROED: bool = true;

    type Handle = &'static LocalAllocator;

    type Error = AllocError;

    fn allocate(
        &self,
        layout: std::alloc::Layout,
        _commit: bool,
    ) -> Result<ferroc::base::Chunk<Self>, Self::Error> {
        bump(S_BASE_ALLOC_CNT, 1);
        bump(S_BASE_ALLOC_BYTES, layout.size() as u64);
        // Object-backed base: a fresh object, mapped, handed to ferroc as one chunk. No talc span,
        // so no in-band free-list metadata is written into the chunk. Monitor keeps talc (its
        // objects are mapped directly and not delete-on-create, so `release_object` would leak).
        // Any decline -- too big for one object, table full, create/map failed -- falls through to
        // the talc path below, which always works.
        // create_and_map asserts sctx != 0; the ferroc fast path can be first-used before the
        // security context is attached, so gate on it here too.
        if USE_OBJECT_BASE
            && layout.align() <= ferroc::config::SLAB_SIZE
            && layout.size() <= OBJ_BASE_USABLE
            && !OUR_RUNTIME.state().contains(RuntimeState::IS_MONITOR)
            && secgate::get_sctx_id().raw() != 0
        {
            // `gate_enter` is `None` for a reentrant grow (the gate call re-entered `allocate` on
            // this thread); such grows fall through to talc rather than gate again and deadlock.
            if let Some(_gate) = gate_enter() {
                if let Some((slot, id)) = super::talc::create_and_map() {
                    let base = slot * MAX_SIZE + OBJ_BASE_SKIP;
                    let mut t = OBJ_BASE.lock().unwrap_or_else(|e| e.into_inner());
                    if t.len < OBJ_BASE_CAP {
                        let n = t.len;
                        t.ents[n] = (slot, id.raw());
                        t.len = n + 1;
                        OBJ_BASE_PRIMED.store(true, core::sync::atomic::Ordering::Release);
                        drop(t);
                        return Ok(unsafe {
                            ferroc::base::Chunk::new(
                                NonNull::new(base as *mut u8).ok_or(AllocError)?,
                                layout,
                                self.local_alloc,
                            )
                        });
                    }
                    // Table full: give the object back and fall through.
                    drop(t);
                    super::talc::release_object(id);
                }
            }
        }
        let ptr = unsafe { self.local_alloc.alloc(layout) };
        // ferroc finds a block's owning slab by masking to SLAB_SIZE (slab.rs:134), and only
        // checks that we honored the requested alignment under `debug_assert!` (arena.rs:123),
        // which is compiled out in release. Verify it on our side of the boundary. Logging rather
        // than asserting: a panic here would re-enter the allocator to format its message.
        if !ptr.is_null() && (ptr as usize) % layout.align() != 0 {
            twizzler_abi::klog_println!(
                "FERROC-BASE-MISALIGN: ptr {:p} size {:x} align {:x}",
                ptr,
                layout.size(),
                layout.align()
            );
        }
        Ok(unsafe {
            ferroc::base::Chunk::new(
                NonNull::new(ptr).ok_or(AllocError)?,
                layout,
                self.local_alloc,
            )
        })
    }

    unsafe fn deallocate(chunk: &mut ferroc::base::Chunk<Self>) {
        bump(1, 1);
        let ptr = chunk.pointer().cast::<u8>().as_ptr();
        let layout = chunk.layout();
        bump(S_BASE_DEALLOC_BYTES, layout.size() as u64);
        // Object-backed chunk: unmap frees it whole (created delete-on-last-unmap), so there is no
        // range to hand back and no talc span to write metadata into.
        if let Some(id) = obj_base_take(ptr as usize / MAX_SIZE) {
            twizzler_abi::klog_println!(
                "OBJBASE-FREE slot {:x} ptr {:p} size {:x}",
                ptr as usize / MAX_SIZE,
                ptr,
                layout.size(),
            );
            super::talc::release_object(id);
            return;
        }
        // The last point at which these frames can be returned. A chunk whose slab came from
        // `SlabSource::Base` is freed by dropping it back to talc and never passes through
        // `decommit` at all, and talc reuses the address range without ever freeing the pages
        // under it -- measured as `l2d`'s 512 pages/iter with `trk.freed` and `tlb_shootdowns`
        // both flat at zero, i.e. the syscall was never reached. ferroc's mmap base gets this for
        // free, since its `deallocate` munmaps. Ordered before `dealloc` so that talc writes its
        // free-list metadata into the range afterwards, faulting it back in.
        unsafe { decommit_range(ptr, layout.size()) };
        chunk.handle.dealloc(ptr, layout);
    }

    unsafe fn commit(&self, ptr: NonNull<[u8]>) -> Result<(), Self::Error> {
        // Nothing to do: a decommitted range faults back in on its own, zeroed.
        let _ = ptr;
        Ok(())
    }

    unsafe fn decommit(&self, ptr: NonNull<[u8]>) {
        bump(0, 1);
        let len = ptr.len();
        unsafe { decommit_range(ptr.cast::<u8>().as_ptr(), len) };
    }
}

ferroc::config!(pub TwzFerroc => TwzFerrocBase: pthread);
