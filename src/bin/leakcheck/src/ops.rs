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
            let reply = if cross { drop(batch); Vec::new() } else { batch };
            if done_tx.send(reply).is_err() {
                break;
            }
        }
    });
    State::Xfree(Xfree { tx: Some(tx), rx, worker: Some(worker) })
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
    State::XfreeWorker(XfreeWorker { go: Some(go), done, worker: Some(worker) })
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

fn l7_run(st: &mut State) {
    for (prog, args) in CHILD_CANDIDATES {
        if std::process::Command::new(prog)
            .args(*args)
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return;
        }
    }
    if let State::Failures(n) = st {
        if *n == 0 {
            crate::console(&format!(
                "LEAKCHECK-SPAWN-ERR l7 PATH={:?}\n",
                std::env::var("PATH").ok()
            ));
            for (prog, args) in CHILD_CANDIDATES {
                let r = match std::process::Command::new(prog).args(*args).output() {
                    Ok(o) => format!("exited {:?}", o.status.code()),
                    Err(e) => format!("{}", e),
                };
                crate::console(&format!("LEAKCHECK-SPAWN-ERR l7 {:?}: {}\n", prog, r));
            }
        }
        *n += 1;
    }
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
    Op { name: "l0-null", setup: no_setup, run: l0_run },
    Op { name: "p1-leak-object", setup: p1_setup, run: p1_run },
    Op { name: "l1a-obj-create-delete", setup: no_setup, run: l1a_run },
    Op { name: "l1b-map-unmap", setup: l1b_setup, run: l1b_run },
    Op { name: "l2a-handle-map-drop", setup: l2a_setup, run: l2a_run },
    Op { name: "l2b-heap", setup: no_setup, run: l2b_run },
    Op { name: "l2c-heap-2mb", setup: no_setup, run: l2c_run },
    Op { name: "l2d-heap-2mb-touched", setup: no_setup, run: l2d_run },
    Op { name: "l2e-heap-small", setup: no_setup, run: l2e_run },
    Op { name: "l2f-heap-2mb-touched-gc", setup: no_setup, run: l2f_run },
    Op { name: "l3-thread", setup: no_setup, run: l3_run },
    Op { name: "l3-thread-addr", setup: no_setup, run: l3_addr_run },
    Op { name: "l3-thread-parts", setup: no_setup, run: l3_parts_run },
    Op { name: "l3-thread-b", setup: no_setup, run: l3_run },
    Op { name: "l3-thread-c", setup: no_setup, run: l3_run },
    Op { name: "l3-thread-slow", setup: no_setup, run: l3_slow_run },
    Op { name: "l3x-xfree-cross", setup: xfree_cross_setup, run: xfree_run },
    Op { name: "l3x-xfree-same", setup: xfree_same_setup, run: xfree_run },
    Op { name: "l3x-xfree-local", setup: no_setup, run: xfree_local_run },
    Op { name: "l3x-xfree-seq", setup: no_setup, run: xfree_seq_run },
    Op { name: "l3x-xfree-worker", setup: xfree_worker_setup, run: xfree_worker_run },
    Op { name: "l7-spawn-proc", setup: l7_setup, run: l7_run },
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
