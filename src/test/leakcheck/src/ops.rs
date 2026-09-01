//! The operation catalogue. Layered cheapest-first: a leak localizes by which layer first shows a
//! slope, and each layer's result is only interpretable against the one below it reading clean.

use std::time::Duration;

use twizzler::object::{ObjectBuilder, TypedObject};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        BackingType, DeleteFlags, LifetimeType, MapFlags, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags, UnmapFlags, sys_object_create, sys_object_ctrl, sys_object_map,
        sys_object_unmap, sys_thread_self_id,
    },
};

pub struct Op {
    pub name: &'static str,
    /// Called once before the measured run, for anything the op needs to own for its lifetime.
    pub setup: fn() -> State,
    pub run: fn(&mut State),
}

pub enum State {
    None,
    Obj(ObjID),
    Leaked(Vec<ObjID>),
    Xfree(Xfree),
    XfreeWorker(XfreeWorker),
    /// Iterations that could not run. See `failures`.
    Failures(usize),
    /// Every child's security context, with whether it still stat'd immediately after that child
    /// exited. Re-stat'd as a set after the post-quiesce -- see [`sctxlive_report`].
    SctxLive(Vec<(ObjID, bool)>),
}

fn no_setup() -> State {
    State::None
}

fn create_obj() -> ObjID {
    let spec = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
        Protections::all(),
    );
    sys_object_create(spec, &[], &[]).expect("leakcheck: object create failed")
}

fn delete_obj(id: ObjID) {
    let _ = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0);
}

// ---- L0: the null control -------------------------------------------------------------------
//
// Pure syscall, allocates nothing, touches no object. Every counter must read a zero slope here.
// If it does not, the instrument is broken -- or leakcheck itself leaks, which is the same
// problem -- and no other row in the report means anything. Its residual is also the floor to
// read the other operations against.

fn l0_run(_: &mut State) {
    std::hint::black_box(sys_thread_self_id());
}

// ---- L0 amplifiers: attribute the 0.14/iter background floor ---------------------------------
//
// The floor is uniform +4-page steps at irregular intervals into the compartment and monitor
// heaps, and it survived both allocator fixes unchanged — so it is either retention on some
// per-sample path or background accrual proportional to wall time. Each arm amplifies one
// candidate by a known factor; the floor scaling with exactly one of them names the path, and
// scaling with none pushes the suspect to what all samples share (count_slots, the console line).
//
// Pre-registered: stats-gate retention -> l0-stats10 reads ~10x floor; kernel-stat retention ->
// l0-kstats10 reads ~10x; time-driven background -> l0-slow500 reads ~2x (it doubles wall per
// iteration); per-iteration-but-elsewhere -> all three read the floor.

fn l0_stats10_run(_: &mut State) {
    for _ in 0..10 {
        std::hint::black_box(monitor_api::stats());
    }
    std::hint::black_box(sys_thread_self_id());
}

/// Kernel stat syscalls only — deliberately not `sys_object_stats`, whose serial dump would add
/// 10x wall and log volume and confound the time arm.
fn l0_kstats10_run(_: &mut State) {
    for _ in 0..10 {
        std::hint::black_box(twizzler_abi::syscall::sys_memory_stats());
        std::hint::black_box(twizzler_abi::syscall::sys_thread_stats());
        std::hint::black_box(twizzler_abi::syscall::sys_sctx_stats());
    }
    std::hint::black_box(sys_thread_self_id());
}

fn l0_slow500_run(_: &mut State) {
    std::thread::sleep(Duration::from_millis(500));
    std::hint::black_box(sys_thread_self_id());
}

// ---- P1: the positive control ---------------------------------------------------------------
//
// Leaks exactly one object per iteration, deliberately: created without DELETE, without a tie,
// never deleted -- which per oleaks.md is what the default constructors do anyway. Its purpose is
// to prove the harness can see a leak of a known size. A report of "no leaks" from a harness that
// has never demonstrated detection is an instrument that answers the same way regardless.

fn p1_setup() -> State {
    State::Leaked(Vec::new())
}

fn p1_run(st: &mut State) {
    let id = create_obj();
    if let State::Leaked(v) = st {
        // Held so nothing can argue the object was collectable; the leak is the point.
        v.push(id);
    }
}

// ---- P2/P3: micro positive controls — the detection-threshold sweep (leakplan §7) -------------
//
// A null verdict is bounded by the smallest leak the instrument has demonstrably seen. These leak
// exactly 64 and 16 touched bytes per iteration into the compartment heap: at N=1000 that is ~15
// and ~4 page-steps respectively, so p2 must classify LEAK and p3 marks where the threshold sits.
// "l0-null flat in the same boot" then certifies the null path to < 64 B/iter, stated rather than
// implied.

fn p2_micro64_run(_: &mut State) {
    std::hint::black_box(Box::leak(Box::new([1u8; 64])));
}

fn p3_micro16_run(_: &mut State) {
    std::hint::black_box(Box::leak(Box::new([1u8; 16])));
}

// ---- L1a: kernel object create + delete ------------------------------------------------------

fn l1a_run(_: &mut State) {
    let id = create_obj();
    delete_obj(id);
}

// ---- L1b: kernel map + unmap of one long-lived object ----------------------------------------

fn l1b_setup() -> State {
    State::Obj(create_obj())
}

fn l1b_run(st: &mut State) {
    let State::Obj(id) = st else { return };
    // Slot 0 is the null page; pick a high fixed slot the runtime will not hand out.
    const SLOT: usize = 200;
    if sys_object_map(None, *id, SLOT, Protections::READ, MapFlags::empty()).is_ok() {
        let _ = sys_object_unmap(None, SLOT, UnmapFlags::empty());
    }
}

// ---- L1c: map + unmap at a fresh slot every iteration -----------------------------------------
//
// The SlotMgr discriminator (regionremodel.md documents that a touched slot's `Box<SlotState>`
// and second-level table frame are only reclaimed at context Drop — "diverges without bound for
// a slot-churning compartment"). `l1b` reuses one fixed slot and is structurally blind to that;
// this op walks a fresh slot per iteration, same single object. SlotMgr-retention predicts
// `trk.kernel_used` growing by table-frame quanta per iteration at high r2 here while `l1b`
// stays flat; both flat pushes the l3-thread kernel signal back toward thread-specific state.

fn l1c_rotate_run(st: &mut State) {
    let State::Obj(id) = st else { return };
    static I: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let slot = 400 + I.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if sys_object_map(None, *id, slot, Protections::READ, MapFlags::empty()).is_ok() {
        let _ = sys_object_unmap(None, slot, UnmapFlags::empty());
    }
}

// ---- L2a: runtime object handle map + drop ---------------------------------------------------

fn l2a_setup() -> State {
    let obj = ObjectBuilder::<u64>::default()
        .build(0u64)
        .expect("leakcheck: builder failed");
    State::Obj(obj.id())
}

fn l2a_run(st: &mut State) {
    let State::Obj(id) = st else { return };
    let obj = twizzler::object::Object::<u64>::map(*id, twizzler_rt_abi::object::MapFlags::READ);
    if let Ok(o) = obj {
        std::hint::black_box(*o.base());
    }
}

// ---- L2b: runtime heap -----------------------------------------------------------------------

fn l2b_run(_: &mut State) {
    let v: Vec<u8> = Vec::with_capacity(64 * 1024);
    std::hint::black_box(v.capacity());
}

// ---- L2c/L2d: large-allocation churn, no threads ---------------------------------------------
//
// The thread leak is ~80 KB/spawn, which is 42x the whole TLS template (1944 bytes measured) and
// 4% of a 2 MiB stack -- so neither is it by size. What a spawn *does* do is take a 2 MiB stack,
// and `stackpool`'s own doc notes that a 2 MiB request "clears ferroc's LARGE_MAX and takes a
// fresh span from the base allocator, whose pages nothing has touched yet".
//
// These two isolate that from threads entirely. `l2b-heap` already allocates 64 KiB per iteration
// and reads clean, so if 2 MiB churn leaks while 64 KiB does not, the boundary is the large-
// allocation path and the thread result is a consequence of it rather than a fact about threads.

fn l2c_run(_: &mut State) {
    let v: Vec<u8> = Vec::with_capacity(2 * 1024 * 1024);
    std::hint::black_box(v.capacity());
}

/// The same size, but touched -- one write per page. Distinguishes "the span is never reused" from
/// "the span is reused but its pages are freshly faulted each time".
fn l2d_run(_: &mut State) {
    let mut v: Vec<u8> = Vec::with_capacity(2 * 1024 * 1024);
    unsafe {
        let p = v.as_mut_ptr();
        for i in (0..2 * 1024 * 1024).step_by(4096) {
            p.add(i).write_volatile(1);
        }
    }
    std::hint::black_box(v.capacity());
}

/// `l2d` again, with an explicit heap collection each iteration.
///
/// The discriminator for whether ferroc's `collect` can reach the pages `l2d` retains *at all*.
/// The decommit syscall is wired into both base-allocator hooks and verified to reach the kernel,
/// but it fires ~5 times per boot, because a freed huge shard goes to ferroc's own free pool
/// rather than back through `Arena::deallocate`. Whether a collection schedule can drain that pool
/// is not answerable by reading: `collect_inner` walks the sized bins and not `huge_shards`, and
/// its `reclaim_all` pops only the *abandoned* list, which a single-threaded loop may never
/// populate.
///
/// So: clean here while `l2d` leaks means a schedule fixes it and only the frequency is open --
/// which is the design question. Both at 512 means no schedule can, because collection never sees
/// this memory, and the fix has to move inside ferroc. Collecting every iteration is deliberately
/// the most generous possible schedule; anything real would collect less often.
fn l2f_run(s: &mut State) {
    l2d_run(s);
    twizzler_rt_abi::core::twz_rt_gc();
}

/// 64 bytes, untouched -- three orders of magnitude below `l2b`'s 64 KiB, and a different
/// allocator path (ferroc's small-object slabs rather than the large-span path `l2c`/`l2d`
/// exercise). Nothing has probed that path.
///
/// It is a discriminator, not just another size. L2 says a *touched* page is never returned by the
/// object holding it -- but that bounds the high-water mark at what was touched, and a live set of
/// 64 bytes can only ever touch one page. So L2 predicts this reads **clean**, exactly as `l2b`
/// does, however many iterations it runs for. A nonzero slope here would mean the allocator keeps
/// advancing into fresh address space instead of handing back the block it just freed, which is a
/// different defect from L2 and a more serious one: growth proportional to *cumulative* allocation
/// volume rather than to the high-water mark of the live set.
///
/// Motivated by a sysbench observation that a 64-byte alloc/free loop appeared to take 978,441
/// frames and return 400 (~4 GB) in one bench interval. That instrument cannot be trusted --
/// `PERFMARK-MEM` is system-wide rather than per-compartment, and it is a delta with no slope, no
/// r2 and no quiesce -- which is what this op is for.
fn l2e_run(_: &mut State) {
    let v: Vec<u8> = Vec::with_capacity(64);
    std::hint::black_box(v.capacity());
}

// ---- L3: thread spawn + join -----------------------------------------------------------------

unsafe extern "C" {
    /// Which heap object backs a pointer. See `runtime/core.rs`.
    fn __twz_rt_diag_heap_id(ptr: *const u8, hi: *mut u64, lo: *mut u64) -> u32;
}

fn heap_id_of(ptr: usize) -> u128 {
    let (mut hi, mut lo) = (0u64, 0u64);
    if unsafe { __twz_rt_diag_heap_id(ptr as *const u8, &mut hi, &mut lo) } == 0 {
        return 0;
    }
    ((hi as u128) << 64) | lo as u128
}

fn l3_run(_: &mut State) {
    let h = std::thread::spawn(|| std::hint::black_box(1u64));
    let _ = h.join();
}

/// Where a spawned thread's first heap allocation lands, per iteration.
///
/// The mechanism test, independent of any slope. ferroc gives each thread-local heap its own
/// context and its own 4 MiB slabs. If a dead thread's id is never recycled, every spawn assigns a
/// *fresh* heap, which takes a *fresh* slab, so consecutive spawns' first allocations march
/// upward -- with a stride on the order of `SLAB_SIZE`. If the id is recycled, the next thread
/// inherits the same heap and the same shard, and the address recurs.
///
/// So this distinguishes the two directly, in addresses rather than in frames: marching means the
/// heap is never reused, repeating means it is. Reported raw, one line per iteration, because the
/// stride is the evidence and a summary statistic would hide it.

/// Ten spawn+joins per iteration.
///
/// The amplifier for the spawn residual: per-spawn retention scales 10x while everything charged
/// per *iteration* -- the sampling syscalls, the census, the harness's own transients -- does not.
/// A residual that reads ten times l3-thread's is per-spawn; one that reads the same is the floor.
fn l3_x10_run(_: &mut State) {
    for _ in 0..10 {
        let h = std::thread::spawn(|| std::hint::black_box(1u64));
        let _ = h.join();
    }
}

/// The same 2,200 spawns as `l3-thread-x10`, spaced 2 ms apart.
///
/// Built as a *density* lever -- `l3-thread` (220 spawns, one per sample) and `l3-thread-x10`
/// (2,200 back to back) differ in spawn count and density at once, so this holds count fixed and
/// removes density.
///
/// **It did not measure density, and the result is the reason.** Run after `l3-thread-x10` it reads
/// 0; run *first* (`track16-records`) it reads 42 and leaves `l3-thread-x10` only 9. So the 0 was
/// an **order** effect: whichever 2,200-spawn op runs first pays the whole high-water fill of
/// `SlotMgr`'s per-slot cells and the rest pay nothing. Both ops set essentially the same record,
/// which is what a density account predicts they would not.
///
/// Keep the op -- as an ordering probe it is exactly what caught the mistake -- but do not cite it
/// for density without running it first in its boot.
fn l3_x10_spaced_run(_: &mut State) {
    for _ in 0..10 {
        let h = std::thread::spawn(|| std::hint::black_box(1u64));
        let _ = h.join();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Same burst as `l3-thread-x10`, then one idle window before the next.
///
/// Separates "no drain opportunity *between* spawns" from "a race between overlapping spawns":
/// both predict `l3-x10-spaced` reads zero, but only the race predicts this arm still reads the
/// residual. Note the op is already sampled after a 4 s quiesce, so simple queueing -- work
/// deferred and later drained -- cannot be the answer either way; what this can still distinguish
/// is a block *orphaned* because a drain window was missed.
fn l3_x10_idle_run(_: &mut State) {
    for _ in 0..10 {
        let h = std::thread::spawn(|| std::hint::black_box(1u64));
        let _ = h.join();
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
}

/// Spawn+join, then burn ~2 ms in *user* mode before the next one.
///
/// The discriminator for the reap gate. `schedule_stattick` reaps at most one exited thread per
/// tick and only when the tick lands on a thread that `is_in_user()`, is not critical, and holds no
/// mutex ([sched.rs](src/kernel/src/processor/sched.rs)); the idle loop's own call fires on every
/// 100th pass. A tight spawn+join loop is in the kernel or blocked for nearly all of its time, so
/// the ticks that would reap keep landing somewhere that skips -- the reap rate is throttled
/// precisely by the thing that produces the backlog.
///
/// Pre-registered: if that gate is the mechanism, this op retains far less per spawn than
/// `l3-thread` despite spawning identically, because it manufactures the user-mode ticks the gate
/// is waiting for. If retention per spawn is unchanged, the gate is not what is limiting the reap
/// and the pacing account is wrong.
fn l3_userspin_run(_: &mut State) {
    let h = std::thread::spawn(|| std::hint::black_box(1u64));
    let _ = h.join();
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_millis(2) {
        std::hint::black_box(0u64);
    }
}

fn l3_addr_run(_: &mut State) {
    static I: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let h = std::thread::spawn(|| {
        let b = Box::new(0u64);
        let a = &*b as *const u64 as usize;
        std::hint::black_box(a)
    });
    let addr = h.join().unwrap_or(0);
    crate::console(&format!(
        "LEAKCHECK-SPAWNADDR {} {:#x}\n",
        I.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        addr
    ));
}

/// The three things a spawn allocates, each with the heap object that pays for it.
///
/// With the thread-destructor fix on, ferroc's per-thread heap stops churning and about half of
/// `l3-thread`'s growth remains -- ~10 pages/iter into a *different* `heap`-noted object, stepped
/// rather than linear. The census names that object but not what puts pages in it. This names it
/// from the allocation side: per spawn, report the address and owning object of the thread's first
/// heap block, its stack, and its TLS region. Whichever one marches is the one paying, and the
/// object id ties it directly to the census row.
fn l3_parts_run(_: &mut State) {
    static I: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let h = std::thread::spawn(|| {
        let b = Box::new(0u64);
        let heap = &*b as *const u64 as usize;
        let local = 0u8;
        let stack = &local as *const u8 as usize;
        let tls = twizzler_abi::syscall::sys_thread_gettls() as usize;
        std::hint::black_box((heap, stack, tls))
    });
    let (heap, stack, tls) = h.join().unwrap_or((0, 0, 0));
    crate::console(&format!(
        "LEAKCHECK-PARTS {} heap={:#x}/{:x} stack={:#x}/{:x} tls={:#x}/{:x}\n",
        I.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        heap,
        heap_id_of(heap),
        stack,
        heap_id_of(stack),
        tls,
        heap_id_of(tls),
    ));
}

// ---- L3 variants: repetition and rate --------------------------------------------------------
//
// `l3b`/`l3c` are `l3` again under different names, so three consecutive runs land in one boot as
// three separate series. talc never returns pages to its heap object, so a single run cannot tell
// live growth from a high-water mark; a second and third run can. Reused freed memory means the
// later runs grow little, and a real retention means they grow the same again.
//
// `l3-slow` runs the identical body an order of magnitude slower. This is the discriminator
// against deferred reclamation: reclamation that merely lags is a race against allocation rate, so
// giving the reaper ten times as long per iteration should cut the slope. A leak is rate-invariant.
// Level-invariance across boots does not test this -- lag proportional to work done looks exactly
// like a linear leak at any starting level.

fn l3_slow_run(st: &mut State) {
    l3_run(st);
    std::thread::sleep(Duration::from_millis(10));
}

// ---- L3x: cross-thread free, no spawning -----------------------------------------------------
//
// The leak18 discriminator: `l3-thread`'s residual is dead-linear at N=220 (9.02/iter, r2 0.995,
// flat by quarters) while `l2e`'s identical-volume single-thread churn is exactly 0.0 -- so
// same-thread churn reuses freed blocks perfectly and something about the thread lifecycle
// defeats reuse. Spawn/join splits alloc and free across threads (main allocates
// `InternalThread`/args, the child or reaper frees; the child allocates the join packet, main
// frees). In a sharded per-thread-heap allocator a foreign free goes to the owning heap's
// deferred list, which only drains when the owner allocates from that shard again.
//
// These two ops isolate that with no spawning at all: one long-lived worker, identical 32 KiB
// batches over the same channels, differing only in who drops the boxes. Cross stranding at churn
// rate with same-free flat convicts the deferred-free path and acquits the rest of the spawn
// machinery. Both arms carry a small cross-freed component regardless (each channel send's node is
// allocated by the sender and freed by the receiver), so a slope on `same` bounds that term.

const XFREE_BATCH: usize = 128; // 128 x 256 B = 32 KiB/iter, ~the fresh-touch rate leak18 measured

type Batch = Vec<Box<[u8; 256]>>;

pub struct Xfree {
    tx: Option<std::sync::mpsc::Sender<Batch>>,
    rx: std::sync::mpsc::Receiver<Batch>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Xfree {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn xfree_setup(cross: bool) -> State {
    let (tx, work_rx) = std::sync::mpsc::channel::<Batch>();
    let (done_tx, rx) = std::sync::mpsc::channel::<Batch>();
    let worker = std::thread::spawn(move || {
        while let Ok(batch) = work_rx.recv() {
            let reply = if cross {
                drop(batch);
                Vec::new()
            } else {
                batch
            };
            if done_tx.send(reply).is_err() {
                break;
            }
        }
    });
    State::Xfree(Xfree {
        tx: Some(tx),
        rx,
        worker: Some(worker),
    })
}

fn xfree_cross_setup() -> State {
    xfree_setup(true)
}

fn xfree_same_setup() -> State {
    xfree_setup(false)
}

fn xfree_run(st: &mut State) {
    let State::Xfree(x) = st else { return };
    let mut batch: Batch = Vec::with_capacity(XFREE_BATCH);
    for i in 0..XFREE_BATCH {
        // `| 1` keeps this off the alloc_zeroed path, whose touch behavior IS_ZEROED changed.
        batch.push(Box::new([(i as u8) | 1; 256]));
    }
    if x.tx.as_ref().unwrap().send(batch).is_ok() {
        // In the same-free arm this drop frees the boxes on the allocating thread.
        drop(x.rx.recv());
    }
}

/// The batch body alone: no worker, no channel, alloc and free on one thread.
///
/// leak19 refuted the cross-thread hypothesis by symmetry -- cross and same-free both strand at
/// 8.90/iter, r2 1.000 -- which also exposed the control it was built on: `l2e` churns *untouched*
/// capacity, and an allocation nothing writes to cannot fault a page however badly reuse fails. So
/// "single-thread churn is clean" was never shown for touched churn. This is that op. Stranding
/// here at ~8.9/iter means touched small-block churn walks forward with no threads involved at
/// all, and every thread result above collapses into that; clean here convicts the channel/second
/// thread specifically.
fn xfree_local_run(_: &mut State) {
    let mut batch: Batch = Vec::with_capacity(XFREE_BATCH);
    for i in 0..XFREE_BATCH {
        batch.push(Box::new([(i as u8) | 1; 256]));
    }
    std::hint::black_box(&batch);
}

/// Identical churn volume to the batch ops, max-live one block: 128 sequential
/// alloc -> touch -> free per iteration.
///
/// The harvest falsifier. leak20 showed the batch strands at churn rate with one thread and no
/// channel, and the shard arithmetic kills the retire-on-full story (128 live x 256 B = 32 KiB,
/// nowhere near a shard). It also un-controls `l0-null`: at ~570 B/iter the floor is consistent
/// with l0's own transient churn marching too -- too small to distinguish reuse from no-reuse. This
/// arm distinguishes them at full signal size. Clean means LIFO reuse works and something about
/// max-live > 1 defeats harvest; stranding at ~8.9 means freed blocks of this class are never
/// harvested at all, and the dive starts at the bump-exhaustion path (`collect_inner` walks sized
/// bins; `reclaim_all` pops only the abandoned list -- see l2f).
fn xfree_seq_run(_: &mut State) {
    for i in 0..XFREE_BATCH {
        let b = Box::new([(i as u8) | 1; 256]);
        std::hint::black_box(&b);
    }
}

/// The seq loop run inside a spawned (THREAD_STARTED) thread; main only signals and waits.
///
/// The discriminator for the main-thread-flag finding: `alloc.rs` routes an allocation to ferroc
/// only when the calling thread has `THREAD_STARTED`, and drops its frees otherwise; the only
/// setters are the spawn trampoline and `cross_compartment_entry`, and `init_core_thread` -- the
/// main thread's path -- is not one of them. If that is the whole story, the identical churn that
/// strands at ~8.6/iter on the main thread (`l3x-xfree-seq`) must read *clean* here, because this
/// worker went through the trampoline. Stranding here instead falsifies the flag story.
///
/// Setup also fingerprints the routing directly: `__twz_rt_diag_heap_id` resolves a pointer via
/// the *normal* talc object list, so a main-thread block (early_talc) reports id 0 while a
/// worker block (ferroc chunk from normal talc) reports its heap object. main=0 + worker!=0 is
/// the flag story in one line.
pub struct XfreeWorker {
    go: Option<std::sync::mpsc::Sender<()>>,
    done: std::sync::mpsc::Receiver<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for XfreeWorker {
    fn drop(&mut self) {
        self.go.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn xfree_worker_setup() -> State {
    let main_box = Box::new([1u8; 256]);
    let main_heap = heap_id_of(&*main_box as *const u8 as usize);
    let (id_tx, id_rx) = std::sync::mpsc::channel::<u128>();
    let (go, go_rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let b = Box::new([1u8; 256]);
        let _ = id_tx.send(heap_id_of(&*b as *const u8 as usize));
        drop(id_tx);
        while go_rx.recv().is_ok() {
            for i in 0..XFREE_BATCH {
                let b = Box::new([(i as u8) | 1; 256]);
                std::hint::black_box(&b);
            }
            if done_tx.send(()).is_err() {
                break;
            }
        }
    });
    let worker_heap = id_rx.recv().unwrap_or(0);
    crate::console(&format!(
        "LEAKCHECK-MAINHEAP main={:x} worker={:x}\n",
        main_heap, worker_heap
    ));
    State::XfreeWorker(XfreeWorker {
        go: Some(go),
        done,
        worker: Some(worker),
    })
}

fn xfree_worker_run(st: &mut State) {
    let State::XfreeWorker(x) = st else { return };
    if x.go.as_ref().unwrap().send(()).is_ok() {
        let _ = x.done.recv();
    }
}

// ---- L7: process spawn + wait ----------------------------------------------------------------
//
// The full stack, and the operation this harness was asked for. `ls` is a symlink init installs at
// /initrd/ls pointing at the on-disk uuhelper, so this needs no initrd entry of its own -- but it
// does need init to have gotten that far. A failed spawn is reported as a skip, never as a clean
// zero: an operation that did not run must not be graded as one that leaked nothing.

/// Spawn leakcheck itself, which exits immediately via `--child-exit`.
///
/// Not `ls`: `exec_spawn`'s `find_id` resolves absolute paths with
/// `twz_rt_resolve_name(Default::default(), ..)`, which does not follow symlinks -- and every route
/// to uuhelper goes through one (`/pkg` -> `/ext/sysroot/pkg`, `/initrd/ls` -> the on-disk copy).
/// Both paths `stat` fine at 126,262,720 bytes and both fail to spawn, so this is a resolver
/// mismatch rather than a missing file: a program can be opened that cannot be exec'd. An initrd
/// object needs no traversal, which is why this one resolves.
///
/// What it measures is unchanged and is the point of the op: a real program loaded into a new
/// compartment through the monitor, run, and reaped.
/// On-disk copy first.
///
/// The namer's `/initrd` namespace holds only what init put there (the coreutils symlinks), not
/// the initrd objects — which is why `/initrd/ls` stats while `/initrd/leakcheck` does not exist
/// to the namer at all, and why both bare and prefixed initrd names fail to resolve once naming is
/// up. `copy_twizzler_build` puts every user binary in `/sysroot/pkg/twizzler/bin`, reachable as
/// `/pkg/...`, which is the path that *did* stat successfully for uuhelper. leakcheck is a few MB
/// there rather than uuhelper's 126, so it also avoids whatever the large-binary path hits.
const CHILD_CANDIDATES: &[(&str, &[&str])] = &[
    ("/pkg/twizzler/bin/leakcheck", &["--child-exit"]),
    ("leakcheck", &["--child-exit"]),
    ("/initrd/leakcheck", &["--child-exit"]),
];

/// How the child is invoked, which is the axis eight path attempts never varied.
///
/// `unittest` spawns children from its own compartment successfully and uses `Command::spawn()`,
/// which leaves stdio inherited. This op used `Command::output()`, and **`output()` forces
/// `Stdio::piped()` on both stdout and stderr** -- a different set of `binding_info` handed to
/// `exec_spawn` through `args.fd_binds`. Every previous investigation held the call form fixed at
/// `output()` and varied the path, so eight paths produced one error because the path was never
/// the variable.
#[derive(Copy, Clone, Debug)]
enum Form {
    /// `unittest`'s exact form: inherited stdio.
    Spawn,
    /// Inherited stdio, but waits like `output()` does.
    Status,
    /// Piped stdout and stderr, null stdin.
    Output,
    /// `output()` changes three things at once, so it names a std method rather than a mechanism.
    /// These three vary one redirection each, everything else inherited.
    NullStdin,
    PipedOut,
    PipedErr,
}

const FORMS: &[Form] = &[
    Form::Spawn,
    Form::Status,
    Form::Output,
    Form::NullStdin,
    Form::PipedOut,
    Form::PipedErr,
];

fn try_form(prog: &str, args: &[&str], form: Form) -> std::io::Result<bool> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    use std::process::Stdio;
    match form {
        Form::Spawn => Ok(cmd.spawn()?.wait()?.success()),
        Form::Status => Ok(cmd.status()?.success()),
        Form::Output => Ok(cmd.output()?.status.success()),
        // The child is `--child-exit` and writes nothing, so an unread pipe cannot fill and block.
        Form::NullStdin => Ok(cmd.stdin(Stdio::null()).spawn()?.wait()?.success()),
        Form::PipedOut => Ok(cmd.stdout(Stdio::piped()).spawn()?.wait()?.success()),
        Form::PipedErr => Ok(cmd.stderr(Stdio::piped()).spawn()?.wait()?.success()),
    }
}

/// Emit the full path x form matrix once per boot, **whether or not the op succeeds**.
///
/// The ladder below short-circuits on the first success, so a passing op says only "some cell
/// works" -- it cannot distinguish "the call form was the bug" from "someone else's fix cured it
/// and the form is irrelevant". Reporting only on failure is the same unpowered-negative shape this
/// harness keeps rediscovering: the diagnostic that would tell you *why* runs exactly when you no
/// longer need it.
static MATRIX_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn report_matrix(tag: &str) {
    for (prog, args) in CHILD_CANDIDATES {
        for form in FORMS {
            let r = match try_form(prog, args, *form) {
                Ok(true) => "ok".to_string(),
                Ok(false) => "ran, nonzero exit".to_string(),
                Err(e) => format!("{}", e),
            };
            crate::console(&format!(
                "LEAKCHECK-SPAWN-MATRIX {} {:?} {:?}: {}\n",
                tag, prog, form, r
            ));
        }
    }
}

fn l7_run(st: &mut State) {
    if !MATRIX_DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        crate::console(&format!(
            "LEAKCHECK-SPAWN-MATRIX begin PATH={:?}\n",
            std::env::var("PATH").ok()
        ));
        report_matrix("probe");
    }
    for (prog, args) in CHILD_CANDIDATES {
        for form in FORMS {
            if try_form(prog, args, *form).unwrap_or(false) {
                return;
            }
        }
    }
    if let State::Failures(n) = st {
        if *n == 0 {
            crate::console(&format!(
                "LEAKCHECK-SPAWN-ERR l7 PATH={:?}\n",
                std::env::var("PATH").ok()
            ));
            // The full matrix, path x form. A row that differs by form and not by path locates the
            // bug in fd binding; one that differs by path and not by form locates it in naming.
            for (prog, args) in CHILD_CANDIDATES {
                for form in FORMS {
                    let r = match try_form(prog, args, *form) {
                        Ok(true) => "ok".to_string(),
                        Ok(false) => "ran, nonzero exit".to_string(),
                        Err(e) => format!("{}", e),
                    };
                    crate::console(&format!(
                        "LEAKCHECK-SPAWN-ERR l7 {:?} {:?}: {}\n",
                        prog, form, r
                    ));
                }
            }
        }
        *n += 1;
    }
}

// ---- L2ctl: positive controls for the userspace heap census ------------------------------------
//
// One allocation of a known size per iteration and nothing else, so the census has a *predicted
// value* to match rather than a predicted sign. A control that only has to come back nonzero
// cannot distinguish a working census from one that counts correctly and buckets wrong; these
// predict the class too.
//
// The sizes are **interior to their class**, not on the boundary. `l2b-heap` allocates exactly
// 65536 and `l2e-heap-small` exactly 64, which are the class edges: if either landed in the
// neighbouring class, "the bucketing is broken" and "the boundary is exclusive and the prediction
// was off by one" would look identical. 49152 and 48 can only land in `le=65536` and `le=64` under
// any boundary convention, so a miss means the mapping is genuinely wrong.
//
// These are new ops rather than edits to `l2b`/`l2e`: those two are in the shared catalogue with
// recorded slopes, and quietly changing what a named op allocates would invalidate every earlier
// reading of it in another session's notes.

/// 48 KiB per iteration: interior to class `le=65536`.
fn l2ctl_48k_run(_: &mut State) {
    let mut v: Vec<u8> = Vec::with_capacity(48 * 1024);
    v.push(0xa5);
    std::hint::black_box(v.as_ptr());
}

/// 48 bytes per iteration: interior to class `le=64`.
fn l2ctl_48b_run(_: &mut State) {
    let mut v: Vec<u8> = Vec::with_capacity(48);
    v.push(0xa5);
    std::hint::black_box(v.as_ptr());
}

// **`black_box(v.as_ptr())`, never `black_box(v.capacity())`, and this is not a style preference.**
//
// The first version of these two ops pinned the capacity, and LLVM deleted the allocation outright:
// `capacity()` is a compile-time constant, nothing else touched the vector, so the whole body
// compiled to storing that constant on the stack and returning. Verified by disassembly, not
// inferred -- `l2ctl_48b_run` contained no `call __rust_alloc` at all. `black_box` on the capacity
// pins a *value*; only making the pointer escape pins the *allocation*.
//
// The same flaw is in `l2b_run`, `l2c_run` and `l2e_run` above, which have been in this catalogue
// for weeks reading "clean". They are left alone deliberately: changing what a named op allocates
// would invalidate every earlier reading of it in another session's notes, silently, because the
// op keeps its name and keeps producing numbers. Reported in `leakcheck.md` instead. `l2d_run`
// touches its allocation and is real -- it is the only one of the four that ever ran.

// ---- L7p: the spawn path, taken apart --------------------------------------------------------
//
// `l7-spawn-proc` retains ~137 KB of this process's own heap per child, and a page count of the
// heap object cannot say which stage of a spawn put it there. A size class (LEAKCHECK-UHEAP) says
// *what* is retained; these say *where*, by running each layer of the spawn on its own against the
// same instrument. The stack, outermost first:
//
//   std `Command` construction  ->  fd layer (open/close, naming)  ->  `exec_spawn` glue
//   ->  `CompartmentLoader` + the monitor round trip  ->  the child itself
//
// An op is only interpretable against the one below it: `l7p-loader` reading the same slope as
// `l7-spawn-proc` puts the whole of it below the fd layer, and `l7p-loader` reading clean while
// the full op leaks puts it in the glue that only `exec_spawn` runs.

const CHILD_PROG: &str = "/pkg/twizzler/bin/leakcheck";

/// Resolve the child's name, and nothing else.
///
/// `exec_spawn` calls this once per spawn through `find_id`. It borrows a pooled naming handle and
/// makes one gate call into `naming-srv`, which is the cheapest thing on the path that crosses a
/// compartment boundary -- so it is also the control for "any gate call retains".
fn l7p_resolve_run(_st: &mut State) {
    let _ = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), CHILD_PROG);
}

/// Build the `Command` std would build, then drop it without spawning.
///
/// Everything above the runtime: argv/env `CString`s, the `CommandEnv` map, and the
/// `current_dir()` lookup `Command::spawn` does on Twizzler to set `TWZ_RT_INITIAL_DIR`. If this
/// leaks, the bug is in libstd's process code or in `env`, not in anything Twizzler-specific.
fn l7p_command_run(_st: &mut State) {
    let mut cmd = std::process::Command::new(CHILD_PROG);
    cmd.args(["--child-exit"]);
    let _ = std::env::current_dir();
    std::hint::black_box(&cmd);
}

/// Open the child binary as a file and close it.
///
/// The fd layer on its own: `kinds::open`, a `FileDesc` into the slot table, the naming lookup
/// behind it, and the close path that takes it back out. A spawn adds one fd (the compartment
/// handle) and this adds one fd (a raw file), so a per-iteration retention here is charged to
/// opening and closing *an* fd rather than to spawning.
fn l7p_fd_run(_st: &mut State) {
    if let Ok(f) = std::fs::File::open(CHILD_PROG) {
        drop(f);
    }
}

/// Read this process's fd bindings the way `build_bindings` and `CompartmentLoader::new` both do.
///
/// Two full passes, because a spawn does it twice -- once in libstd to build the child's stdio
/// bindings and once inside `CompartmentLoader::new`, which loads the *parent's* binds and then
/// has them overwritten by `with_fd_specs`. That second one is pure waste on every spawn and is a
/// candidate in its own right.
fn l7p_binds_run(_st: &mut State) {
    for _ in 0..2 {
        let mut v = vec![twizzler_rt_abi::bindings::binding_info::default(); 8];
        loop {
            let n = twizzler_rt_abi::fd::twz_rt_fd_read_binds(&mut v);
            if n < v.len() {
                v.truncate(n);
                break;
            }
            v.extend_from_slice(&[twizzler_rt_abi::bindings::binding_info::default(); 8]);
        }
        std::hint::black_box(&v);
    }
}

/// Load a compartment through the monitor and wait for it to exit -- no libstd, no fd layer.
///
/// This is `exec_spawn` minus its glue: the same `CompartmentLoader`, the same monitor gate calls,
/// the same child. What it does *not* do is build a `Command`, read fd bindings, open a
/// compartment fd, or run libstd's wait loop. Waiting rather than dropping the handle immediately
/// is not incidental: dropping it would let 220 children exist at once and measure scheduling
/// pressure instead of retention.
fn l7p_loader_run(st: &mut State) {
    use monitor_api::{CompartmentFlags, CompartmentLoader, NewCompartmentFlags};
    let Ok(id) = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), CHILD_PROG) else {
        if let State::Failures(n) = st {
            *n += 1;
        }
        return;
    };
    let mut loader = CompartmentLoader::new(
        CHILD_PROG,
        CHILD_PROG,
        id.into(),
        NewCompartmentFlags::empty(),
    );
    loader.args(["--child-exit"]);
    let Ok(comp) = loader.load() else {
        if let State::Failures(n) = st {
            *n += 1;
        }
        return;
    };
    let mut flags = match comp.info() {
        Ok(i) => i.flags,
        Err(_) => {
            if let State::Failures(n) = st {
                *n += 1;
            }
            return;
        }
    };
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }
}

/// Does a child's security-context object still `stat` after the child has exited?
///
/// This is the predicate `secgate::util::HandleMgr::gc_handles` uses to decide that a compartment
/// is gone and its handle table can be dropped:
///
/// ```ignore
/// fn sctx_still_valid(id: &ObjID) -> bool { sys_object_stat(*id).is_ok() }
/// self.handles.retain(|id, sv| !sv.is_empty() && sctx_still_valid(id));
/// ```
///
/// If it keeps returning `Ok` after the compartment is dead, that GC can never fire, every dead
/// child's naming handle stays in the table forever, and each retained handle pins a
/// `NameSession` -> `Arc<dyn Namespace>` -> that namespace's `NsCache`, which `GlobalCache`'s
/// eviction then refuses to drop (it only evicts caches with `strong_count == 1`). The service's
/// own comment says the excess is "bounded by the number of client sessions" -- so an unbounded
/// session count is an unbounded cache.
///
/// **Thread reap can take up to two seconds**, so a short delay cannot distinguish "never reaped"
/// from "not reaped yet" -- and the first version of this probe used 50 ms and concluded the
/// predicate was wrong at steady state. It was measuring the reap latency it had failed to wait
/// for. The op therefore records every child's sctx as it goes and re-stats the whole set from
/// `sctxlive_report`, which runs *after* the op's post-quiesce (4 s by default, and far longer than
/// that for every child but the last). The immediate per-iteration stat is kept only as the
/// contrast: immediate-alive with settled-gone is a healthy deferred reap, and only
/// settled-alive is a defect.
fn l7p_sctxlive_run(st: &mut State) {
    use monitor_api::{CompartmentFlags, CompartmentLoader, NewCompartmentFlags};
    use twizzler_abi::syscall::sys_object_stat;

    let State::SctxLive(seen) = st else {
        return;
    };
    let Ok(id) = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), CHILD_PROG) else {
        return;
    };
    let mut loader = CompartmentLoader::new(
        CHILD_PROG,
        CHILD_PROG,
        id.into(),
        NewCompartmentFlags::empty(),
    );
    loader.args(["--child-exit"]);
    let Ok(comp) = loader.load() else {
        return;
    };
    let Ok(info) = comp.info() else {
        return;
    };
    let sctx = info.sctx;
    let mut flags = info.flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }
    // Drop the handle first: while this compartment handle is live the monitor has every reason to
    // keep the context around, and a stat that succeeds then says nothing about the GC's case.
    drop(comp);

    let now_ok = sys_object_stat(sctx).is_ok();
    if seen.len() < 5 {
        crate::console(&format!(
            "LEAKCHECK-SCTXLIVE i={} sctx={:x} stat_immediately_after_exit={}\n",
            seen.len(),
            sctx.raw(),
            if now_ok { "ok" } else { "gone" },
        ));
    }
    seen.push((sctx, now_ok));
}

fn l7p_sctxlive_setup() -> State {
    State::SctxLive(Vec::new())
}

/// The tally, printed once by the op that owns it, **after the post-quiesce**. See
/// [`l7p_sctxlive_run`].
///
/// `settled` is the number whose security context still stats once every deferred reap has had its
/// chance. That is the figure that says whether `HandleMgr::gc_handles`'s predicate is wrong;
/// `immediate` says only how often the reap had not finished yet, which is not a defect.
pub fn sctxlive_report(st: &State) {
    let State::SctxLive(seen) = st else {
        return;
    };
    if seen.is_empty() {
        return;
    }
    use twizzler_abi::syscall::sys_object_stat;
    let immediate = seen.iter().filter(|(_, ok)| *ok).count();
    let settled = seen
        .iter()
        .filter(|(sctx, _)| sys_object_stat(*sctx).is_ok())
        .count();
    crate::console(&format!(
        "LEAKCHECK-SCTXLIVE-TOTAL n={} alive_immediately={} alive_after_quiesce={}\n",
        seen.len(),
        immediate,
        settled
    ));
}

fn l7_setup() -> State {
    State::Failures(0)
}

/// How many iterations of this op did not actually run. Non-zero invalidates the op's row.
pub fn failures(st: &State) -> usize {
    match st {
        State::Failures(n) => *n,
        _ => 0,
    }
}

pub const OPS: &[Op] = &[
    Op {
        name: "l0-null",
        setup: no_setup,
        run: l0_run,
    },
    Op {
        name: "l0-stats10",
        setup: no_setup,
        run: l0_stats10_run,
    },
    Op {
        name: "l0-kstats10",
        setup: no_setup,
        run: l0_kstats10_run,
    },
    Op {
        name: "l0-slow500",
        setup: no_setup,
        run: l0_slow500_run,
    },
    Op {
        name: "p1-leak-object",
        setup: p1_setup,
        run: p1_run,
    },
    Op {
        name: "p2-microleak-64",
        setup: no_setup,
        run: p2_micro64_run,
    },
    Op {
        name: "p3-microleak-16",
        setup: no_setup,
        run: p3_micro16_run,
    },
    Op {
        name: "l1a-obj-create-delete",
        setup: no_setup,
        run: l1a_run,
    },
    Op {
        name: "l1b-map-unmap",
        setup: l1b_setup,
        run: l1b_run,
    },
    Op {
        name: "l1c-map-unmap-slots",
        setup: l1b_setup,
        run: l1c_rotate_run,
    },
    Op {
        name: "l2a-handle-map-drop",
        setup: l2a_setup,
        run: l2a_run,
    },
    Op {
        name: "l2b-heap",
        setup: no_setup,
        run: l2b_run,
    },
    Op {
        name: "l2c-heap-2mb",
        setup: no_setup,
        run: l2c_run,
    },
    Op {
        name: "l2d-heap-2mb-touched",
        setup: no_setup,
        run: l2d_run,
    },
    Op {
        name: "l2e-heap-small",
        setup: no_setup,
        run: l2e_run,
    },
    Op {
        name: "l2f-heap-2mb-touched-gc",
        setup: no_setup,
        run: l2f_run,
    },
    Op {
        name: "l3-thread",
        setup: no_setup,
        run: l3_run,
    },
    Op {
        name: "l3-thread-x10",
        setup: no_setup,
        run: l3_x10_run,
    },
    Op {
        name: "l3-x10-spaced",
        setup: no_setup,
        run: l3_x10_spaced_run,
    },
    Op {
        name: "l3-x10-idle",
        setup: no_setup,
        run: l3_x10_idle_run,
    },
    Op {
        name: "l3-thread-userspin",
        setup: no_setup,
        run: l3_userspin_run,
    },
    Op {
        name: "l3-thread-addr",
        setup: no_setup,
        run: l3_addr_run,
    },
    Op {
        name: "l3-thread-parts",
        setup: no_setup,
        run: l3_parts_run,
    },
    Op {
        name: "l3-thread-b",
        setup: no_setup,
        run: l3_run,
    },
    Op {
        name: "l3-thread-c",
        setup: no_setup,
        run: l3_run,
    },
    Op {
        name: "l3-thread-slow",
        setup: no_setup,
        run: l3_slow_run,
    },
    Op {
        name: "l3x-xfree-cross",
        setup: xfree_cross_setup,
        run: xfree_run,
    },
    Op {
        name: "l3x-xfree-same",
        setup: xfree_same_setup,
        run: xfree_run,
    },
    Op {
        name: "l3x-xfree-local",
        setup: no_setup,
        run: xfree_local_run,
    },
    Op {
        name: "l3x-xfree-seq",
        setup: no_setup,
        run: xfree_seq_run,
    },
    Op {
        name: "l3x-xfree-worker",
        setup: xfree_worker_setup,
        run: xfree_worker_run,
    },
    Op {
        name: "l7-spawn-proc",
        setup: l7_setup,
        run: l7_run,
    },
    Op {
        name: "l2ctl-48k",
        setup: no_setup,
        run: l2ctl_48k_run,
    },
    Op {
        name: "l2ctl-48b",
        setup: no_setup,
        run: l2ctl_48b_run,
    },
    Op {
        name: "l7p-resolve",
        setup: no_setup,
        run: l7p_resolve_run,
    },
    Op {
        name: "l7p-command",
        setup: no_setup,
        run: l7p_command_run,
    },
    Op {
        name: "l7p-fd",
        setup: no_setup,
        run: l7p_fd_run,
    },
    Op {
        name: "l7p-binds",
        setup: no_setup,
        run: l7p_binds_run,
    },
    Op {
        name: "l7p-loader",
        setup: l7_setup,
        run: l7p_loader_run,
    },
    Op {
        name: "l7p-sctxlive",
        setup: l7p_sctxlive_setup,
        run: l7p_sctxlive_run,
    },
    // The same op as `l7-spawn-proc`, run again after `l7p-loader`. Ordering is a live confounder
    // in this harness: whichever spawn-heavy op runs first pays any one-time fill, so "the part
    // leaks as much as the whole" and "the part ran first" produce the same table. Two readings of
    // the identical op on either side of the parts separate them.
    Op {
        name: "l7-spawn-proc-b",
        setup: l7_setup,
        run: l7_run,
    },
];

/// The phase-1 default: the two controls plus the cheap kernel layers. Nothing here depends on
/// naming, the pager, or another program being in the initrd.
pub const DEFAULT_OPS: &[&str] = &[
    "l0-null",
    "p1-leak-object",
    "l1a-obj-create-delete",
    "l1b-map-unmap",
    "l2a-handle-map-drop",
    "l2b-heap",
];
