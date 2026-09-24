//! Large zeroed allocations -- anonymous `mmap` -- served from dedicated objects.
//!
//! mlibc's `sys_vm_map` turns `mmap(MAP_ANONYMOUS)` into `twz_rt_malloc(size, 0x1000,
//! ZERO_MEMORY)`, and rustc's `ensure_sufficient_stack` issues ~81,000 of them per `cargo build`,
//! every one exactly 1 MiB plus stacker's two guard pages. Serving those from the general heap
//! makes the runtime *produce* the zeroes -- as a memset of the whole mapping, or as a per-mapping
//! kernel zero-range call measured end to end at 19.6 us. Fresh object pages are already zero, so a
//! mapping carved from never-used arena space needs neither.
//!
//! An arena object is a bitmap of [`CHUNK`]-sized chunks plus a second bitmap recording which of
//! them have ever been handed out. An allocation is a first-fit search for a contiguous run of
//! clear bits; a free clears them again and leaves the dirty bits alone. First fit means a freed
//! run is reused before untouched space is, so an arena settles at the working set rather than
//! growing to its capacity -- and reuse is exactly when the zeroing has to happen, which is the one
//! `sys_object_copy` in [`alloc`]. [`pick`] claims the run under the lock and [`alloc`] issues that
//! call after dropping it, so the syscall is never inside a critical section other stripes spin on.
//!
//! Two things here are load-bearing, both learned by getting them wrong in an earlier version of
//! this that lived in mlibc:
//!
//! - **Stripe.** Bumping every mapping out of one object concentrates a whole compartment's anon
//!   faulting onto that object's page-table lock, which is a *sleeping* mutex -- so the losers park
//!   rather than spin, and the cost appears as idle cpus and wakeups, not as cpu time. Measured:
//!   contended acquisitions of that lock went from 0 to 11,201, and the build got 18% slower.
//!   Striping by thread puts concurrent faulters on different objects, hence different locks. This
//!   is why a stripe that cannot fit its request creates a *new* object rather than borrowing
//!   another stripe's.
//! - **Zero as late as possible.** A freed chunk is dirty and `MAP_ANONYMOUS` must read as zero, so
//!   reuse needs zeroing -- but doing it eagerly on free is exactly the 19.6 us per-mapping call
//!   this exists to remove. So a free records nothing but the cleared bits, and a chunk is zeroed
//!   only at the moment it is handed out again. Untouched chunks are zero by construction and cost
//!   nothing at all.
//!
//! **Predecessor.** This replaces a bump-pointer arena that preferred virgin space and recycled by
//! zeroing its whole used prefix once the bump ran out. Its measurements are worth keeping because
//! they bound what any policy here can buy. Against a kernel whose `zero_range` walked its whole
//! range, four rewind budgets and a retire arm all landed within the +/-2s run-to-run spread of the
//! no-arena path, and wall time rose monotonically with batch size while bytes zeroed stayed fixed:
//! amortizing the syscall is the wrong axis, because a `zero_range` holds the object's page-table
//! mutex for the walk. Against the current O(resident) walk, 32x the batch bought 2% of the bytes,
//! and *retiring* the object -- 29x fewer bytes zeroed, since the replacement's pages are virgin --
//! ran 14% slower on non-overlapping ranges, because every page then faults back in. Fewer bytes
//! zeroed is not the objective function; keeping pages mapped and resident is.

use core::sync::atomic::{AtomicBool, Ordering};

use monitor_api::RuntimeThreadControl;
use secgate::{get_sctx_id, get_thread_id};
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_copy, sys_object_create, sys_object_ctrl, BackingType, CreateTieFlags,
        CreateTieSpec, DeleteFlags, LifetimeType, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_rt_abi::object::{MapFlags, Protections};

use crate::{runtime::RuntimeState, OUR_RUNTIME};

/// Requests smaller than this keep the ordinary heap path: below it the fixed cost of an arena
/// chunk is not obviously cheaper than what the allocator already does. Must stay <= the
/// `LAZY_ZERO_MIN` gate in `syms.rs`, or an allocation served from here would be freed to the heap.
pub const SEG_MIN: usize = 128 * 1024;
/// Allocation granularity. The workload this exists for maps 1 MiB + two guard pages, so it costs
/// a second chunk whose pages are never touched -- and therefore never resident, so the zero walk
/// skips them.
const CHUNK: usize = 1024 * 1024;
/// Chunks per arena object: 512 MiB, comfortably inside `MAX_SIZE` with both metadata pages.
const ARENA_CHUNKS: usize = 512;
const BITMAP_WORDS: usize = ARENA_CHUNKS / 64;
/// Independent bump targets. Concurrent faulters land on different objects, hence different
/// page-table locks -- see the module docs.
const NR_STRIPES: usize = 8;
const MAX_ARENAS: usize = 32;

type Bitmap = [u64; BITMAP_WORDS];

#[derive(Clone, Copy)]
struct Arena {
    slot: usize,
    id: ObjID,
    /// Set: chunk is handed out.
    alloc: Bitmap,
    /// Set: chunk has been handed out at some point and its contents are not known to be zero.
    /// Only ever cleared by a zeroing `sys_object_copy`, which is immediately followed by handing
    /// the chunk out again -- so in practice this is "not virgin".
    dirty: Bitmap,
}

fn bit(bm: &Bitmap, i: usize) -> bool {
    bm[i / 64] & (1u64 << (i % 64)) != 0
}

fn set_range(bm: &mut Bitmap, start: usize, n: usize) {
    for i in start..start + n {
        bm[i / 64] |= 1u64 << (i % 64);
    }
}

fn clear_range(bm: &mut Bitmap, start: usize, n: usize) {
    for i in start..start + n {
        bm[i / 64] &= !(1u64 << (i % 64));
    }
}

/// First run of `n` clear bits. First fit, so previously-used low chunks are found before
/// untouched high ones. Whole words that are entirely free or entirely taken are skipped without
/// looking at their bits, which is most of them once an arena has a working set.
fn find_run(bm: &Bitmap, n: usize) -> Option<usize> {
    let mut run = 0usize;
    for (w_i, &w) in bm.iter().enumerate() {
        if w == 0 {
            run += 64;
            if run >= n {
                return Some((w_i + 1) * 64 - run);
            }
            continue;
        }
        if w == u64::MAX {
            run = 0;
            continue;
        }
        for b in 0..64 {
            if w & (1u64 << b) == 0 {
                run += 1;
                if run >= n {
                    return Some(w_i * 64 + b + 1 - run);
                }
            } else {
                run = 0;
            }
        }
    }
    None
}

/// Outermost dirty chunks within `[start, start + n)`, so one call covers them all. Clean chunks
/// caught in the middle are already zero, and zeroing them again is proportional to their resident
/// pages, of which they have none.
fn dirty_span(bm: &Bitmap, start: usize, n: usize) -> Option<(usize, usize)> {
    let first = (start..start + n).find(|&i| bit(bm, i))?;
    let last = (first..start + n).rev().find(|&i| bit(bm, i))?;
    Some((first, last))
}

struct Inner {
    arenas: [Option<Arena>; MAX_ARENAS],
    /// Index into `arenas` of each stripe's current target.
    cur: [Option<usize>; NR_STRIPES],
}

/// A raw spinlock, not a `Mutex`: this runs inside the allocator, and taking a lock that can
/// allocate or touch TLS from here is the re-entrancy hazard the rest of this module's neighbours
/// document at length. The critical sections are a bitmap scan over 8 words and the call rate is a
/// few thousand a second.
static LOCK: AtomicBool = AtomicBool::new(false);
static mut INNER: Inner = Inner {
    arenas: [None; MAX_ARENAS],
    cur: [None; NR_STRIPES],
};

struct Guard;

fn lock() -> Guard {
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    Guard
}

impl Drop for Guard {
    fn drop(&mut self) {
        LOCK.store(false, Ordering::Release);
    }
}

fn inner() -> &'static mut Inner {
    // SAFETY: every caller holds `LOCK`.
    unsafe { &mut *core::ptr::addr_of_mut!(INNER) }
}

fn base_of(slot: usize) -> usize {
    slot * MAX_SIZE + NULLPAGE_SIZE
}

fn make_arena() -> Option<Arena> {
    // Same shape as the heap's own span objects (`talc::create_and_map`): volatile, tied to this
    // compartment's context, and marked for deletion straight away so that dropping the mapping is
    // what actually frees it.
    let cc = get_sctx_id();
    if cc.raw() == 0 {
        return None;
    }
    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
            Protections::all(),
        ),
        &[],
        &[CreateTieSpec::new(cc, CreateTieFlags::empty()).into()],
    )
    .ok()?;
    let slot = monitor_api::monitor_rt_object_map(id, MapFlags::READ | MapFlags::WRITE)
        .ok()?
        .slot;
    let _ = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0);
    Some(Arena {
        slot,
        id,
        alloc: [0; BITMAP_WORDS],
        dirty: [0; BITMAP_WORDS],
    })
}

/// A claimed run, carried out of the critical section so the zeroing syscall can run without it.
struct Claim {
    arena: usize,
    id: ObjID,
    slot: usize,
    chunk: usize,
    /// Byte offset and length within the object that must read as zero before this is handed out.
    zero: Option<(usize, usize)>,
}

/// Take `n` chunks in `a`. The bits are set here, so the run belongs to this caller the moment the
/// lock drops; whether it still needs zeroing is the caller's problem.
fn claim(a: &mut Arena, idx: usize, n: usize) -> Option<Claim> {
    let c = find_run(&a.alloc, n)?;
    let zero =
        dirty_span(&a.dirty, c, n).map(|(first, last)| (first * CHUNK, (last + 1 - first) * CHUNK));
    set_range(&mut a.alloc, c, n);
    set_range(&mut a.dirty, c, n);
    Some(Claim {
        arena: idx,
        id: a.id,
        slot: a.slot,
        chunk: c,
        zero,
    })
}

/// Pick an arena and claim a run in it. Runs under the lock; the only syscall it can reach is
/// `make_arena`, which is cold once the stripes have their objects.
fn pick(inner: &mut Inner, stripe: usize, n: usize) -> Option<Claim> {
    if let Some(i) = inner.cur[stripe] {
        if let Some(a) = inner.arenas[i].as_mut() {
            if let Some(c) = claim(a, i, n) {
                return Some(c);
            }
        }
    }
    // A fresh object beats another stripe's: sharing one is sharing its page-table lock.
    if let Some(idx) = inner.arenas.iter().position(|a| a.is_none()) {
        match make_arena() {
            Some(a) => {
                inner.arenas[idx] = Some(a);
                inner.cur[stripe] = Some(idx);
                if let Some(c) = claim(inner.arenas[idx].as_mut()?, idx, n) {
                    return Some(c);
                }
            }
            None => {}
        }
    }
    // Table full. Sharing an object costs lock contention; declining costs the mapping the whole
    // per-mapping zero-range this exists to remove, so share.
    for i in 0..MAX_ARENAS {
        if inner.cur[stripe] == Some(i) {
            continue;
        }
        if let Some(a) = inner.arenas[i].as_mut() {
            if let Some(c) = claim(a, i, n) {
                inner.cur[stripe] = Some(i);
                return Some(c);
            }
        }
    }
    None
}

/// Serve `len` bytes of zeroed, page-aligned memory, or `None` to fall back to the heap.
///
/// `len` must be a whole number of pages.
pub fn alloc(len: usize) -> Option<*mut u8> {
    let n = len.div_ceil(CHUNK);
    if len == 0 || n > ARENA_CHUNKS || !OUR_RUNTIME.state().contains(RuntimeState::READY) {
        return None;
    }
    // Everything below can reach the monitor -- `monitor_rt_object_map` is a secgate call, and a
    // gate call saves and restores the thread pointer. With no control block installed yet that
    // reads `%fs:0` and faults, which is the hazard `talc::create_and_map` documents for
    // `MONDEBUG`. `get_thread_id`/`get_sctx_id` guard themselves (they fall back to a syscall
    // when `get_tp()` is zero); the gate call does not.
    if unsafe { dynlink::tls::get_current_thread_control_block::<RuntimeThreadControl>() }.is_null()
    {
        return None;
    }
    let stripe = stripe_of();
    let c = {
        let _g = lock();
        pick(inner(), stripe, n)
    }?;
    // Deliberately outside the lock. In steady state every allocation reuses dirty chunks, so
    // holding the arena lock across this would put a syscall in a critical section that every
    // other stripe spins on -- and the run is already claimed, so nothing else can touch it.
    if let Some((off, zlen)) = c.zero {
        let src = ObjectSource::new_zero((NULLPAGE_SIZE + off) as u64, zlen);
        if sys_object_copy(c.id, &[src.into()]).is_err() {
            // Memory that cannot be cleaned must not be handed out, so give the run back. The
            // dirty bits stay set: the contents are still whatever they were.
            let _g = lock();
            if let Some(a) = inner().arenas[c.arena].as_mut() {
                clear_range(&mut a.alloc, c.chunk, n);
            }
            return None;
        }
    }
    Some((base_of(c.slot) + c.chunk * CHUNK) as *mut u8)
}

/// Give a segment back. Returns false if `ptr` did not come from here.
pub fn free(ptr: *mut u8, len: usize) -> bool {
    let addr = ptr as usize;
    let slot = addr / MAX_SIZE;
    if slot == 0 {
        return false;
    }
    let _g = lock();
    let inner = inner();
    let Some(i) = inner
        .arenas
        .iter()
        .position(|a| a.as_ref().is_some_and(|a| a.slot == slot))
    else {
        return false;
    };
    let a = inner.arenas[i].as_mut().unwrap();
    // Dirty bits stay set: the zero they imply is owed to whoever takes these chunks next, and
    // paying it here is the per-mapping cost this module exists to avoid.
    let off = addr - base_of(slot);
    let n = len.div_ceil(CHUNK);
    if off % CHUNK == 0 && off / CHUNK + n <= ARENA_CHUNKS {
        clear_range(&mut a.alloc, off / CHUNK, n);
    }
    // The slot is ours either way, so the heap must not see this pointer.
    true
}

/// Which target this thread uses. Any stable per-thread value works; the point is only that two
/// threads faulting at once usually land on different objects.
fn stripe_of() -> usize {
    get_thread_id().raw() as usize % NR_STRIPES
}
