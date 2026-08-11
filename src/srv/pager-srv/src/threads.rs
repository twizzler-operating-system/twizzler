use std::{
    cell::{Cell, RefCell},
    future::Future,
    ops::Range,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    task::{Context, Poll, Wake, Waker},
    thread::{available_parallelism, JoinHandle},
    time::Instant,
};

use async_executor::LocalExecutor;
use twizzler_abi::{
    pager::{CompletionToKernel, KernelCommand, ObjectEvictFlags, PagerFlags, RequestFromKernel},
    syscall::{
        sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
        ThreadSyncSleep, ThreadSyncWake,
    },
};
use twizzler_queue::{QueueError, ReceiveFlags, SubmissionFlags};

use crate::{
    nvme::controller::MAX_DATA_QUEUES, request_handle::handle_kernel_request, watchdog, PAGER_CTX,
};

/// Worker threads per core. Measured: at 1x the blocking NVMe leaf loses to the async one by 2.1x,
/// and at 2x it wins back the intra-worker overlap it gave up (pagerperf.md 1).
const WORKER_SCALE: usize = 2;

/// Fast-lane depth at which a fast request starts eyeing an idle bulk lane.
///
/// One outstanding item is the lane doing its job -- borrowing there drains the reservation into
/// bulk on essentially every request, since `DepthGuard` keeps the count raised for the whole life
/// of a detached page-in. At two, something is genuinely queued behind the in-flight item, which is
/// the queueing the reservation exists to prevent.
const FAST_BORROW_DEPTH: usize = 2;

/// Most fast lanes to reserve, however large the pool gets. The reservation exists to keep small
/// demand faults off bulk transfers, which one or two lanes achieve; past that it is just bulk
/// capacity taken away.
const MAX_FAST_LANES: usize = 2;

/// Watchdog owner name for a lane, so a dump says which kind of worker wedged.
///
/// Leaked, once per worker at pool construction: the watchdog wants `'static` and the pool is built
/// exactly once, so this is a bounded handful of allocations rather than a growing leak. It used to
/// be a pair of fixed arrays, whose *lengths* silently became the cap on pool size.
fn lane_name(fast: bool, index: usize) -> &'static str {
    let name = if fast {
        format!("fast{}", index)
    } else {
        format!("worker{}", index)
    };
    Box::leak(name.into_boxed_str())
}

/// Largest page-data request a fast lane will accept. Three shapes reach us from
/// `Object::ensure_in_core_pager`: a single page (a meta page, or a tail), a run capped at 64, and
/// a whole 512-page large-page fill when the level-1 entry is empty. 16 pages (64 KiB) keeps the
/// first shape and the small tails, and leaves anything where the transfer itself dominates -- the
/// 256 KiB runs and the 2 MiB fills -- on a bulk lane, which is the traffic the reservation exists
/// to get out from behind.
const FAST_PAGE_LIMIT: usize = 16;

/// Whether a request belongs on a reserved fast lane. `DramPages` is pure bookkeeping and
/// `ObjectInfoReq` is a `len()` probe the length cache usually answers without touching the disk
/// (see pagerperf.md 12). Page data qualifies only when it is small and not a prefetch: prefetch
/// ranges run to the whole object, which is exactly the traffic the reservation exists to dodge.
/// Create/delete/evict all do synchronous filesystem work under the global `fs` lock, so they are
/// bulk by definition.
fn is_fast(req: &RequestFromKernel) -> bool {
    match req.cmd() {
        KernelCommand::ObjectInfoReq(_) | KernelCommand::DramPages(_) => true,
        KernelCommand::PageDataReq(_, range, flags) => {
            !flags.contains(PagerFlags::PREFETCH) && range.page_count() <= FAST_PAGE_LIMIT
        }
        KernelCommand::ObjectCreate(..) | KernelCommand::ObjectDel(_) => false,
        KernelCommand::ObjectEvict(_) => false,
    }
}

pub struct WorkItem {
    start: Instant,
    qid: u32,
    req: RequestFromKernel,
}

impl WorkItem {
    fn new(qid: u32, req: RequestFromKernel) -> Self {
        Self {
            start: Instant::now(),
            qid,
            req,
        }
    }
}

pub struct WorkerThread {
    _handle: JoinHandle<()>,
    pending: async_channel::Sender<WorkItem>,
    /// Work owed by this lane: queued items plus items being handled plus any task they detached.
    /// Read by the dequeue thread to pick a lane, so it is deliberately not the channel length --
    /// see [`DepthGuard`].
    depth: Arc<AtomicUsize>,
}

#[thread_local]
static LOCAL_EXEC: LocalExecutor<'static> = LocalExecutor::new();

/// The running worker's depth counter, so work detached onto this executor can keep counting
/// against the lane that owns it. `None` on the dequeue thread and at init, where `spawn_async`
/// and `run_async` are also called but no lane is being charged.
#[thread_local]
static LANE_DEPTH: RefCell<Option<Arc<AtomicUsize>>> = RefCell::new(None);

/// This worker's nvme queue pair, so workers never contend on one submission queue.
///
/// Non-worker threads report 0 and share worker 0's queue. That is deliberate rather than a
/// fallback: the dequeue thread only handles non-fence evicts inline, which just record a range and
/// never touch the disk, and init is single-threaded, so neither issues enough traffic to earn a
/// queue.
#[thread_local]
static QUEUE_INDEX: Cell<usize> = Cell::new(0);

pub fn current_queue_index() -> usize {
    QUEUE_INDEX.get()
}

/// Number of worker threads, fixed on first call.
///
/// Memoized because the nvme controller sizes its queue pairs from this and is initialized before
/// the pool exists -- the two have to agree, and `available_parallelism` is not contractually
/// stable across calls.
/// How many nvme queue pairs to ask the device for.
///
/// `WORKER_SCALE` per core, capped by `MAX_DATA_QUEUES`. Memoized because the controller is
/// initialized before the pool exists and the two have to agree, and because
/// `available_parallelism` is not contractually stable across calls.
///
/// A blocking worker is parked for the whole transfer, so the pool -- not the executor -- is what
/// keeps commands outstanding; 1x cores measured 2.1x worse than 2x (pagerperf.md 1). Floor of 2 so
/// the fast lane always has somewhere to live.
pub fn desired_queues() -> usize {
    static NR: OnceLock<usize> = OnceLock::new();
    *NR.get_or_init(|| {
        (available_parallelism().unwrap().get() * WORKER_SCALE).clamp(2, MAX_DATA_QUEUES)
    })
}

/// Queue pairs the device actually granted, recorded by the controller during init.
static GRANTED_QUEUES: OnceLock<usize> = OnceLock::new();

/// Record the negotiated queue count. Called once from `nvme::controller`, before the pool is
/// built.
pub fn set_granted_queues(n: usize) {
    let _ = GRANTED_QUEUES.set(n);
}

/// Number of worker threads: **one per nvme queue pair**.
///
/// A waiting thread reaps the queue it submitted to, so 1-1 gives each worker an uncontended
/// requester lock and no spurious wakeups -- with several workers per queue, one queue's interrupt
/// wakes them all and most find the completion belongs to someone else. Sharing is *correct*
/// (`pagerplan.md`, "waiters reap"), just noisier, which is why this follows the device down rather
/// than leaving the pool at what we asked for.
///
/// `desired_queues()` is the fallback for configurations with no nvme controller at all (the
/// virtio-mem store), where nothing ever records a grant.
pub fn nr_workers() -> usize {
    GRANTED_QUEUES
        .get()
        .copied()
        .unwrap_or_else(desired_queues)
        .max(2)
}

/// Holds a lane's depth raised for the lifetime of one unit of work.
///
/// The reason this exists rather than a bare fetch_add/fetch_sub pair: page-data and fence-sync
/// requests `spawn_async` a detached task and return, so the work item finishes while the paging it
/// asked for is still running. Releasing the count there would show a worker buried in detached
/// page-ins as idle and send it more. The detached task takes its own guard before the item's is
/// dropped, so the count never dips between the two.
pub struct DepthGuard(Option<Arc<AtomicUsize>>);

impl DepthGuard {
    /// Take over a count already raised by the dispatcher when it queued the item.
    fn adopt(depth: Arc<AtomicUsize>) -> Self {
        Self(Some(depth))
    }

    /// Raise a fresh count against the lane running this code, if any.
    fn acquire() -> Self {
        let depth = LANE_DEPTH.borrow().clone();
        if let Some(depth) = &depth {
            depth.fetch_add(1, Ordering::Relaxed);
        }
        Self(depth)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        if let Some(depth) = self.0.take() {
            depth.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl WorkerThread {
    fn new(index: usize, name: &'static str) -> Self {
        let (send, recv) = async_channel::bounded::<WorkItem>(32);
        let depth = Arc::new(AtomicUsize::new(0));
        let thread_depth = depth.clone();
        Self {
            _handle: std::thread::spawn(move || {
                LANE_DEPTH.replace(Some(thread_depth.clone()));
                QUEUE_INDEX.set(index);
                loop {
                    // `run_async`, not a bare park: detached tasks from earlier requests are still
                    // on this executor and their completions still land on this
                    // thread's queue.
                    let wi = run_async(recv.recv()).unwrap();
                    let _depth = DepthGuard::adopt(thread_depth.clone());
                    tracing::trace!(
                        "{}: starting handling after {}us",
                        wi.qid,
                        wi.start.elapsed().as_micros()
                    );

                    let work = watchdog::begin(name, wi.qid, wi.req);
                    let resp = run_async(handle_kernel_request(
                        PAGER_CTX.get().unwrap(),
                        wi.qid,
                        wi.req,
                        &work,
                    ));
                    tracing::trace!(
                        "{}: done handling after {}us",
                        wi.qid,
                        wi.start.elapsed().as_micros()
                    );
                    work.phase("notify-kernel");
                    for resp in resp {
                        PAGER_CTX
                            .get()
                            .unwrap()
                            .kernel_notify
                            .complete(wi.qid, resp, SubmissionFlags::empty())
                            .unwrap();
                    }
                }
            }),
            pending: send,
            depth,
        }
    }

    /// Charge the lane before queueing, so a dispatch decision can never race ahead of the item it
    /// just placed. Gives the item back if the lane is full.
    fn try_send(&self, wi: WorkItem) -> Result<(), WorkItem> {
        self.depth.fetch_add(1, Ordering::Relaxed);
        self.pending.try_send(wi).map_err(|e| {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            e.into_inner()
        })
    }

    fn send_blocking(&self, wi: WorkItem) {
        self.depth.fetch_add(1, Ordering::Relaxed);
        self.pending.send_blocking(wi).expect("pager worker exited");
    }
}

pub struct Workers {
    threads: Vec<WorkerThread>,
    /// `threads[..nr_fast]` are reserved for requests [`is_fast`] accepts, so a one-page fault
    /// never queues behind a multi-megabyte prefetch. Fixed at startup, which is what lets the
    /// fence hash in `dispatch_ordered` stay stable for the life of the process.
    nr_fast: usize,
    /// Rotating start for the idle-bulk-lane search, so a burst of borrows spreads instead of
    /// stacking on the lowest-index lane. The depth charge lands after the decision is read, so a
    /// fixed start makes concurrent dispatches all pick the same "idle" lane.
    borrow_rotor: AtomicUsize,
}

impl Workers {
    fn new() -> Self {
        let nr_threads = nr_workers();
        // One reserved lane keeps cheap requests moving; a second only starts paying once there are
        // enough bulk lanes left that reserving it does not just move the queue. Never take the
        // last lane -- bulk work has to have somewhere to go.
        let nr_fast = (nr_threads / 4)
            .clamp(1, MAX_FAST_LANES)
            .min(nr_threads - 1);
        let threads = (0..nr_threads)
            .map(|index| {
                let name = if index < nr_fast {
                    lane_name(true, index)
                } else {
                    lane_name(false, index - nr_fast)
                };
                WorkerThread::new(index, name)
            })
            .collect();
        Self {
            threads,
            nr_fast,
            borrow_rotor: AtomicUsize::new(0),
        }
    }

    /// An idle bulk lane, starting the search at a rotating offset.
    fn idle_bulk_lane(&self) -> Option<usize> {
        let bulk = self.lane(false);
        let n = bulk.len();
        if n == 0 {
            return None;
        }
        let start = self.borrow_rotor.fetch_add(1, Ordering::Relaxed) % n;
        (0..n)
            .map(|k| bulk.start + (start + k) % n)
            .find(|i| self.threads[*i].depth.load(Ordering::Relaxed) == 0)
    }

    fn lane(&self, fast: bool) -> Range<usize> {
        if fast {
            0..self.nr_fast
        } else {
            self.nr_fast..self.threads.len()
        }
    }

    /// Queue an item on the least-loaded lane of its class. Depth counts detached tasks, so a
    /// worker that already returned from its work item but is still paging for it still reads as
    /// loaded; ties break to the lowest index, and the charge lands before the next decision reads
    /// it, so a run of identical requests still spreads.
    fn dispatch(&self, wi: WorkItem) {
        let fast = is_fast(&wi.req);
        let lane = self.lane(fast);
        let depth = |i: usize| self.threads[i].depth.load(Ordering::Relaxed);
        let mut best = lane
            .clone()
            .min_by_key(|i| depth(*i))
            .expect("every request class needs at least one lane");
        // A fast request waiting behind another fast request is the queueing the reservation was
        // meant to prevent, and a bulk lane at depth 0 is not the bulk transfer it exists to get
        // out from behind -- so borrow it. Only in this direction: bulk work never takes a fast
        // lane, or the reservation would stop meaning anything.
        if fast && depth(best) >= FAST_BORROW_DEPTH {
            if let Some(idle) = self.idle_bulk_lane() {
                best = idle;
            }
        }
        // Spill before blocking: an idle sibling beats stalling the dequeue thread, and stalling
        // beats the panic a failed `try_send` used to be. Fast work may fall back onto *idle* bulk
        // lanes too -- an empty one is not the bulk transfer the reservation exists to avoid -- but
        // never onto a busy one, which would be the queueing it exists to prevent.
        let mut wi = wi;
        let siblings = lane.filter(|i| *i != best);
        let idle_bulk = self
            .lane(false)
            .filter(|i| fast && *i != best && depth(*i) == 0);
        for idx in std::iter::once(best).chain(siblings).chain(idle_bulk) {
            match self.threads[idx].try_send(wi) {
                Ok(()) => return,
                Err(item) => wi = item,
            }
        }
        self.threads[best].send_blocking(wi);
    }

    /// Queue an item on a lane chosen by `hash` instead of by depth, for streams that have to stay
    /// ordered. Never spills, since another lane would reorder it -- it waits for its own.
    fn dispatch_ordered(&self, wi: WorkItem, hash: u64) {
        let lane = self.lane(false);
        let idx = lane.start + (hash as usize) % lane.len();
        if let Err(wi) = self.threads[idx].try_send(wi) {
            self.threads[idx].send_blocking(wi);
        }
    }
}

pub struct PagerThreadPool {
    _workers: Arc<Workers>,
    _kq_handler: JoinHandle<()>,
}

impl PagerThreadPool {
    pub fn new(
        queue: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
    ) -> Self {
        let pool = Arc::new(Workers::new());
        PagerThreadPool {
            _workers: pool.clone(),
            _kq_handler: std::thread::spawn(move || kq_handler_main(pool, queue)),
        }
    }
}

/// Detach work onto this thread's executor, keeping the lane charged for it. The guard is taken
/// before the caller's own is dropped, so the lane never momentarily reads as idle while it still
/// owes this work.
pub fn spawn_async<O: 'static>(f: impl Future<Output = O> + 'static) {
    let depth = DepthGuard::acquire();
    LOCAL_EXEC
        .spawn(async move {
            let _depth = depth;
            f.await
        })
        .detach();
}

/// Per-thread park word, and the `Waker` that signals it.
///
/// This is the whole point of the custom parker: `async_io::block_on` parks on a global `Reactor`
/// behind a `try_lock`, so a completion has to be noticed by whichever thread holds that lock and
/// then handed to the thread whose executor owns the task. Parking on our own word lets a
/// completion wake the thread that is going to poll the task, directly.
struct ThreadPark {
    word: AtomicU64,
}

impl ThreadPark {
    fn signal(&self) {
        self.word.store(1, Ordering::SeqCst);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.word),
                usize::MAX,
            ))],
            None,
        );
    }

    fn sleep_op(&self) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.word),
            0,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

impl Wake for ThreadPark {
    fn wake(self: Arc<Self>) {
        self.signal();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.signal();
    }
}

#[thread_local]
static PARK: RefCell<Option<Arc<ThreadPark>>> = RefCell::new(None);

fn new_park() -> Arc<ThreadPark> {
    Arc::new(ThreadPark {
        word: AtomicU64::new(0),
    })
}

/// Drive `f` to completion on this thread, sleeping on our own park word and -- when this thread
/// has an nvme queue -- that queue's interrupt word, in one `sys_thread_sync`.
///
/// Waking on the interrupt directly is what removes the handoff: this thread reaps its own queue,
/// which marks the request ready and signals the very waker we are about to poll. Arming both words
/// is what makes it safe either way, since another thread reaping first still signals our park
/// word.
///
/// Note this deliberately does not drive `async_io`'s reactor. Nothing on this path needs it -- the
/// only reactor-backed futures left in the pager are one behind `if false` and `Disk::yield_now`,
/// which no longer uses a timer.
fn park_poll<O>(f: impl Future<Output = O>) -> O {
    // A nested call must not share the outer call's word. The loop below clears it before every
    // poll, so an inner loop would swallow a wake meant for the future the outer one is parked on
    // and strand it -- and nesting is normal here (`Disk::run_async` and `data.rs`'s
    // `read_physical_pages` both run inside a future this is already driving). Holding the
    // `RefMut` for the duration is what makes a re-entrant call take the fresh-parker arm;
    // `async_io::block_on` does exactly this, for exactly this reason.
    // `cached` is held (not dropped) for the whole call on purpose -- that is the nesting guard.
    let mut cached = PARK.try_borrow_mut().ok();
    let park = match cached.as_mut() {
        Some(slot) => slot.get_or_insert_with(new_park).clone(),
        None => new_park(),
    };
    let waker = Waker::from(park.clone());
    let mut cx = Context::from_waker(&waker);
    let mut f = core::pin::pin!(f);
    loop {
        // Clear before polling, so a wake landing during the poll is seen below rather than lost.
        park.word.store(0, Ordering::SeqCst);
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        // Drain first: a completion already sitting in the queue must not be slept on. This also
        // consumes the interrupt word, without which the sleep below would never block.
        crate::nvme::reap_current_queue();
        if park.word.load(Ordering::SeqCst) != 0 {
            continue;
        }
        let mut ops = heapless::Vec::<ThreadSync, 2>::new();
        let _ = ops.push(ThreadSync::new_sleep(park.sleep_op()));
        if let Some(int) = crate::nvme::current_queue_sleep() {
            let _ = ops.push(ThreadSync::new_sleep(int));
        }
        let _ = sys_thread_sync(&mut ops, None);
        crate::nvme::reap_current_queue();
    }
}

/// Run `f` on this thread's executor, so anything already detached here keeps making progress.
pub fn run_async<O: 'static>(f: impl Future<Output = O>) -> O {
    park_poll(LOCAL_EXEC.run(f))
}

/// Run `f` and *only* `f` -- no executor, so no unrelated task can be polled from inside this call.
///
/// For lwext4's block-device callbacks, which always run with `Ext4Store::fs` held. Driving the
/// shared executor there polls some other pager task on this thread, and any of them that reaches
/// for `fs` -- a non-reentrant std mutex already held further up this very stack -- blocks the
/// thread forever. See pagerperf.md 2 for the post-mortem.
pub fn run_isolated<O>(f: impl Future<Output = O>) -> O {
    park_poll(f)
}

fn kq_handler_main(
    workers: Arc<Workers>,
    queue: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
) {
    loop {
        let mut tmp = heapless::Vec::<(u32, RequestFromKernel), 8>::new();
        while !tmp.is_full() {
            let res = queue.receive(ReceiveFlags::NON_BLOCK);
            match res {
                Ok((id, req)) => unsafe { tmp.push_unchecked((id, req)) },
                Err(e) if e == QueueError::WouldBlock => {
                    if !tmp.is_empty() {
                        break;
                    }
                    if let Ok((id, req)) = queue.receive(ReceiveFlags::empty()) {
                        unsafe { tmp.push_unchecked((id, req)) };
                    }
                }
                Err(e) => {
                    tracing::error!("queue recieve error: {}", e);
                }
            }
        }

        for (id, req) in tmp {
            // Evicts never go through `dispatch`: a non-fence evict only records the page in
            // `PerObjectInner::sync_map`, and the fence that follows it drains and writes out what
            // was recorded, so the record has to happen first. Handling non-fence evicts inline
            // here is what guarantees that -- they complete before anything later in this batch is
            // even queued.
            if let KernelCommand::ObjectEvict(evict) = req.cmd() {
                // Send all per-object evict streams to one thread so they preserve order.
                if evict.flags.contains(ObjectEvictFlags::FENCE) {
                    let hash = req.id().map_or(0, |x| x.parts()[0] ^ x.parts()[1]) + id as u64;
                    workers.dispatch_ordered(WorkItem::new(id, req), hash);
                } else {
                    // Handled inline, so anything that blocks here stops the pager dequeuing from
                    // the kernel at all -- worth naming separately from a stuck worker.
                    let work = watchdog::begin("kq-handler-inline", id, req);
                    let resp = run_async(handle_kernel_request(
                        PAGER_CTX.get().unwrap(),
                        id,
                        req,
                        &work,
                    ));
                    work.phase("notify-kernel");
                    for resp in resp {
                        PAGER_CTX
                            .get()
                            .unwrap()
                            .kernel_notify
                            .complete(id, resp, SubmissionFlags::empty())
                            .unwrap();
                    }
                }
            } else {
                workers.dispatch(WorkItem::new(id, req));
            }
        }
    }
}

pub struct Waiter<T: Send> {
    data: Mutex<(Option<T>, Option<Waker>)>,
}

impl<T: Send> Default for Waiter<T> {
    fn default() -> Self {
        Self {
            data: Mutex::new((None, None)),
        }
    }
}

impl<T: Send> Waiter<T> {
    pub fn finish(&self, item: T) {
        let mut data = self.data.lock().unwrap();
        data.0 = Some(item);
        if let Some(w) = data.1.take() {
            w.wake();
        }
    }
}

impl<T: Send> Future for &Waiter<T> {
    type Output = T;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut data = self.data.lock().unwrap();
        if data.0.is_some() {
            std::task::Poll::Ready(data.0.take().unwrap())
        } else {
            data.1.replace(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}
