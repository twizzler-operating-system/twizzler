use alloc::{collections::BTreeMap, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use inflight::{Inflight, InflightManager};
use itertools::Itertools;
use request::ReqKind;
use twizzler_abi::{
    object::ObjID,
    pager::{PagerFlags, PhysRange},
    syscall::ObjectCreate,
};
use twizzler_rt_abi::{
    bindings::sync_info,
    error::{ResourceError, TwzError},
};

use crate::{
    condvar::CondVar,
    instant::Instant,
    memory::{
        context::virtmem::region::MapRegion,
        frame::{PHYS_LEVEL_LAYOUTS, get_frame},
        tracker::FrameAllocFlags,
    },
    mutex::{LockGuard, Mutex},
    obj::{
        LookupFlags, ObjectRef, PageNumber, PtGuard,
        pagetables::{DirtyList, ObjectPageTable},
    },
    once::{Once, OnceWait},
    processor::sched::{SchedFlags, schedule},
    spinlock::Spinlock,
    syscall::sync::finish_blocking,
    thread::{
        current_thread_ref,
        entry::start_new_kernel,
        priority::{Priority, PriorityClass},
    },
};

mod inflight;
pub(crate) mod profile;
mod queues;
mod request;

pub use profile::print_pager_profile;
pub use queues::init_pager_queue;
pub use request::Request;

pub const MAX_PAGER_OUTSTANDING_FRAMES: usize = 65536;
pub const DEFAULT_PAGER_OUTSTANDING_FRAMES: usize = 1024 * 16;

/// A/B: shard the inflight manager by object id. `false` routes every selection to shard 0, which
/// reproduces the single-mutex behaviour with the sharded code compiled in -- one tree state, both
/// arms.
pub const INFLIGHT_SHARDED: bool = true;

/// Shard count. Object ids are content-derived hashes, so their low bits index shards uniformly
/// with no hash step (the same argument `obj::omap` makes).
const INFLIGHT_SHARDS: usize = 16;

pub(super) struct ShardedInflight {
    shards: [Mutex<InflightManager>; INFLIGHT_SHARDS],
}

static INFLIGHT_MGR: OnceWait<ShardedInflight> = OnceWait::new();

fn inflight_mgr() -> &'static ShardedInflight {
    INFLIGHT_MGR.call_once(|| ShardedInflight {
        shards: core::array::from_fn(|_| Mutex::new(InflightManager::new())),
    })
}

/// Which shard owns requests for `id`. `None` -- only [`ReqKind::Pages`], the pager-memory
/// donation request, which names no object -- goes to shard 0.
fn shard_idx(id: Option<ObjID>) -> usize {
    if !INFLIGHT_SHARDED {
        return 0;
    }
    match id {
        Some(id) => (id.raw() as usize) % INFLIGHT_SHARDS,
        None => 0,
    }
}

/// Take the inflight-manager lock, timing the acquisition.
///
/// One global mutex guards every request's admission, coalescing, wait setup and completion, so it
/// is on both the submit and the completion path of every page-in and taken again on each turn of
/// the wait loop. Whether that serializes is a question worth asking only now that requests
/// actually overlap; the timing is an `Instant::now()` pair, which is two `rdtsc`s.
pub(super) fn lock_shard(idx: usize) -> LockGuard<'static, InflightManager> {
    let start = crate::instant::Instant::now();
    let guard = inflight_mgr().shards[idx].lock();
    profile::PAGER_PROFILE.mgr_lock((crate::instant::Instant::now() - start).as_nanos() as u64);
    guard
}

/// The shard owning `rk`.
///
/// Every operation on a request -- admission, coalescing, narrowing, wait setup, completion -- is
/// keyed by the same object, so one shard hold covers a whole submit or completion sequence just
/// as the single lock did. The one thing that must *not* be per-shard is admission, which is why
/// `LIVE` is a global atomic.
pub(super) fn lock_inflight_for(rk: &ReqKind) -> LockGuard<'static, InflightManager> {
    lock_shard(shard_idx(rk.objid()))
}

pub(super) fn lock_inflight_for_obj(id: ObjID) -> LockGuard<'static, InflightManager> {
    lock_shard(shard_idx(Some(id)))
}

/// Page-in errors the pager reported, held until the thread that was waiting can be told.
///
/// An error completion carries DONE, so the request is removed and its slot cleared before the
/// waiter it woke gets to run -- leaving that waiter unable to tell "the pages arrived" from "the
/// pager gave up". It returns Ok with the pages still absent, the faulting instruction retries,
/// and the fault repeats forever; `log_fault`'s refault counter can see that loop, but nothing
/// ends it. This bridges the gap so the fault can become a fault.
///
/// Deliberately *not* "pages absent after completion", which is a normal outcome: a prefetch the
/// pager declines is acked DONE with no pages, and the retry that follows is by design. Only an
/// error recorded here fails the wait, because only an error is reproduced by retrying.
///
/// Taken rather than read, so one error faults one waiter and a fault arriving later starts clean
/// rather than inheriting a failure that may well have been transient.
static PAGE_IN_ERRORS: Mutex<BTreeMap<ObjID, TwzError>> = Mutex::new(BTreeMap::new());

pub(super) fn record_page_in_error(id: ObjID, err: TwzError) {
    PAGE_IN_ERRORS.lock().insert(id, err);
}

fn take_page_in_error(id: ObjID) -> Option<TwzError> {
    PAGE_IN_ERRORS.lock().remove(&id)
}

/// Why a create failed, for the syscall that asked for it.
///
/// [cmd_object] waits for the completion but reports nothing, so a create the pager rejected used
/// to return `Ok(id)` for an object that exists nowhere -- and since the pager path returns before
/// `register_object`, the failure surfaced only at the *first map*, as `NoSuchObject`, in unrelated
/// code. Five sweep failures over ten days were attributed to four different tests that way.
static CREATE_ERRORS: Mutex<BTreeMap<ObjID, TwzError>> = Mutex::new(BTreeMap::new());

pub(super) fn record_create_error(id: ObjID, err: TwzError) {
    CREATE_ERRORS.lock().insert(id, err);
}

fn take_create_error(id: ObjID) -> Option<TwzError> {
    CREATE_ERRORS.lock().remove(&id)
}

pub fn check_timed_out_requests() {
    // Runs on the idle thread. Calling inflight_mgr() here would let the *lowest priority* thread
    // in the system become the Once initializer; a higher-priority thread reaching
    // inflight_mgr() while that is in flight spins in Once::poll without yielding, and on a
    // uniprocessor the idle thread then never runs again -- a silent, permanent boot hang right
    // after "pager ready". Nothing here needs to force initialization.
    if !INFLIGHT_MGR.is_complete() {
        return;
    }
    if !crate::pager::inflight::pager_ready() {
        return;
    }
    // One shard at a time, never two at once: this runs from the idle loop and holds nothing
    // across shards, so it cannot convoy the submit paths.
    for idx in 0..INFLIGHT_SHARDS {
        lock_shard(idx).check_timed_out_requests();
    }
}

/// A/B knob for the speculative prefetch below. Setting it false reproduces the pre-prefetch path
/// exactly, which is what makes any measurement of it one rebuild apart.
const PREFETCH_ON_LOOKUP: bool = false;

/// Speculatively page in an object's first region, just after the pager first described it.
///
/// Issued from the thread that did the waiting, never from the completion thread: this can block
/// donating memory to the pager, and the completion thread is the only one draining completions.
///
/// The request covers the first large-page region minus its first page -- page zero is the null
/// page and is never backed. That also keeps every page of it off the large-page branch in
/// `pager_compl_handle_page_data`, which only fires on a 2MB-aligned object page.
///
/// Nothing waits on this: `get_pages_and_wait` skips the wait for a prefetch, so a failure here
/// costs the caller nothing but the submission.
fn prefetch_first_region(obj: &ObjectRef) {
    if !PREFETCH_ON_LOOKUP || !obj.use_pager() {
        return;
    }
    let pages = PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE - 1;
    let mut used_pager = false;
    let _ = ensure_in_core(
        obj,
        obj.lock_page_tables(),
        &[(PageNumber::base_page(), pages)],
        PagerFlags::PREFETCH,
        true,
        &mut used_pager,
        None,
    );
}

/// A/B knob for the map-time prefetch below, in the habit of the one above.
const PREFETCH_ON_MAP: bool = false;

/// Start the page-in for the region *after* the one the object's first fault will read ahead into.
///
/// **It has to start past that window, and this is the whole design.** Seeding pages inside it does
/// not help the first fault -- it *disables* it. `ensure_in_core_pager` widens a touch to a whole
/// read-ahead window only when the region is untouched (`is_empty_at_level(.., 1)`), so 16 seeded
/// pages made the first fault read that test as "region already in use", abandon the 1024-page
/// request, and fall back to per-page probing: faults went 87-103 -> 117-324 on `pagepar`, page-in
/// calls doubled, and fewer regions arrived contiguously enough to merge into large pages. The
/// map-time seed and the fault-time read-ahead are coupled through that one predicate, and the only
/// way to have both is to keep them on disjoint regions.
///
/// So: the first fault owns `[0, READAHEAD_REGIONS)` and this owns the window after it, driven
/// through the same widening from that region's first page. Same code, so the request is byte-for-
/// byte the one a fault landing there would issue -- which means when the reader does arrive it
/// coalesces onto this in `add_request` rather than duplicating it.
///
/// Issued without [PagerFlags::PREFETCH] on purpose: that flag is what the pager routes and caps
/// on, and `speculative` carries the only part the kernel needs (nobody is blocked, so do not
/// wait, and do not spend to make it succeed).
///
/// Gated on the object being longer than that window. Without the gate a small file's prefetch is
/// entirely past EOF, which commits holes rather than data -- the failure `pagerperf.md` 18
/// suspected of the reverted extent-warming attempt.
pub fn prefetch_on_map(obj: &ObjectRef) {
    if !PREFETCH_ON_MAP || !obj.use_pager() || !obj.claim_map_prefetch() {
        return;
    }
    let window = (PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE)
        * crate::obj::Object::READAHEAD_REGIONS;
    let start = PageNumber::from_offset(window * PageNumber::PAGE_SIZE);
    match obj.known_len() {
        Some(len) if len > start.as_byte_offset() as u64 => {}
        // Either not a sized object at all, or one that ends inside the first fault's own window.
        // Both are nothing to speculate about.
        _ => {
            mapprefetch::skipped();
            return;
        }
    }
    mapprefetch::issued();
    let _ = obj.ensure_in_core_pager(
        obj.lock_page_tables(),
        start,
        1,
        &mut false,
        &mut false,
        PagerFlags::empty(),
        true,
    );
}

/// Whether the map-time prefetch fires at all, and how often it finds the head already there.
///
/// It has to be counted here rather than read off the pager's counters, because clearing
/// [PagerFlags::PREFETCH] is precisely what makes these requests indistinguishable from demand
/// faults on the wire -- `REQSTATS` will call every one of them demand.
mod mapprefetch {
    use core::sync::atomic::{AtomicU64, Ordering};

    static ISSUED: AtomicU64 = AtomicU64::new(0);
    static SKIPPED: AtomicU64 = AtomicU64::new(0);

    pub fn skipped() {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
    }

    pub fn issued() {
        let n = ISSUED.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            log::info!(
                "MAPPREFETCH: {} issued, {} skipped (too short, or not a sized object)",
                n,
                SKIPPED.load(Ordering::Relaxed),
            );
        }
    }
}

pub fn lookup_object_and_wait(id: ObjID) -> Option<ObjectRef> {
    if id.raw() == 0 {
        return None;
    }
    // Only prefetch for an object we actually had to ask the pager about. A lookup that hits
    // in-kernel on the first pass is the common case by far (`pagerperf.md` 17: 27 of 256 maps
    // reached the pager at all), and speculating on every one of those would be speculating on
    // objects that have been resident for a long time already.
    let mut asked_pager = false;
    let entered = Instant::now();
    loop {
        let iter_start = Instant::now();
        let lo = crate::obj::lookup_object(id, LookupFlags::empty());
        log::trace!("lookup_object_and_wait: id = {}, result = {:?}", id, lo);
        let looked_up = Instant::now();
        profile::lookupstats::iteration((looked_up - iter_start).as_nanos() as u64);
        match lo {
            crate::obj::LookupResult::Found(arc) => {
                profile::lookupstats::finished(
                    (looked_up - entered).as_nanos() as u64,
                    !asked_pager,
                );
                if asked_pager {
                    prefetch_first_region(&arc);
                }
                return Some(arc);
            }
            crate::obj::LookupResult::WasDeleted => return None,
            crate::obj::LookupResult::NotFound => {
                if crate::obj::is_no_exist(id) {
                    return None;
                }
            }
            _ => {}
        }

        if !crate::pager::inflight::pager_ready() {
            return None;
        }
        let slot_gen = crate::pager::inflight::slot_gen();
        let mut mgr = lock_inflight_for_obj(id);
        let Ok(inflight) = mgr.add_request(ReqKind::new_info(id)) else {
            log::warn!("out of pager request slots");
            drop(mgr);
            crate::pager::inflight::wait_for_slot(slot_gen);
            continue;
        };
        drop(mgr);
        inflight.for_each_pager_req(None, |pager_req| {
            queues::submit_pager_request(pager_req, None, inflight.rk().clone());
        });
        asked_pager = true;
        let submitted = Instant::now();
        profile::lookupstats::submitted((submitted - looked_up).as_nanos() as u64);

        let mut mgr = lock_inflight_for(inflight.rk());
        let thread = current_thread_ref().unwrap();
        if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
            drop(mgr);
            crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
            finish_blocking(guard);
        };
        let woke = Instant::now();
        profile::lookupstats::blocked((woke - submitted).as_nanos() as u64);
        profile::lookupstats::woke(
            submitted.into_time_span().as_nanos() as u64,
            woke.into_time_span().as_nanos() as u64,
        );
    }
}

/// Tag a request with the priority class of the thread that needs it.
///
/// Only "is this below ordinary userspace" is conveyed, not the priority itself: the pager uses it
/// to keep low-priority paging off the lanes it reserves for demand faults, which is a routing
/// decision, not a scheduling one. Full priority inheritance through the pager wants the requesting
/// thread's actual priority and is a larger design (see `pagerplan.md` stage 4).
///
/// Applied where the wire request is built, not where the [ReqKind] is -- see the note at that call
/// site for why this must stay out of the coalescing key.
pub(super) fn requester_flags() -> PagerFlags {
    let background =
        current_thread_ref().is_some_and(|t| t.effective_priority().class < PriorityClass::User);
    if background {
        PagerFlags::BACKGROUND
    } else {
        PagerFlags::empty()
    }
}

/// How often a page-data wait ends early because the caller's own pages arrived, against how often
/// it runs to the end of the request.
///
/// `early` is the whole point of passing a required range down: it counts faults that no longer
/// sleep through the widening. `parks` over `waits` says what the extra per-batch wakeups cost --
/// a waiter that is woken and finds its pages still missing pays one pass round the loop.
mod waitstats {
    use core::sync::atomic::{AtomicU64, Ordering};

    static WAITS: AtomicU64 = AtomicU64::new(0);
    static EARLY: AtomicU64 = AtomicU64::new(0);
    static PARKS: AtomicU64 = AtomicU64::new(0);

    pub fn park() {
        PARKS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn finish(early: bool) {
        if early {
            EARLY.fetch_add(1, Ordering::Relaxed);
        }
        let n = WAITS.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            log::info!(
                "PAGERWAIT: {} waits, {} ended early on the required range, {} parks",
                n,
                EARLY.load(Ordering::Relaxed),
                PARKS.load(Ordering::Relaxed),
            );
        }
    }
}

/// Are all of `[page, page + len)` backed in `obj` right now?
///
/// Takes the object's page-table lock, so it must not be called holding it.
fn pages_present(obj: &ObjectRef, page: PageNumber, len: usize) -> bool {
    let mut pt = obj.lock_page_tables();
    (0..len).all(|i| !pt.is_empty_at_level(page.offset(i).as_byte_offset() as u64, 0))
}

/// `speculative` says nobody is blocked on this request. It used to be read off
/// [PagerFlags::PREFETCH], which conflated two independent things: what the *pager* should do with
/// the request (route it to a bulk lane, cap and decline it) and whether the *caller* waits. A
/// map-time head fetch wants the second without the first.
fn submit_page_request<'a>(
    obj: &'a ObjectRef,
    mut page: PageNumber,
    mut len: usize,
    flags: PagerFlags,
    speculative: bool,
    mut tree: PtGuard<'a>,
    used_pager: &mut bool,
    required: Option<(PageNumber, usize)>,
    out: &mut heapless::Vec<Inflight, MAX_REQ_BATCH>,
) -> Result<PtGuard<'a>, TwzError> {
    // Drop the pages at the head of this range that the object has acquired since the range was
    // built. `ensure_in_core_pager` filters against the page tables under the lock, but
    // `ensure_in_core` then drops that lock and can *block* in `provide_pager_memory` waiting for
    // the pager to ack a donation, and every range after the first waits here as well -- so by the
    // time a range is submitted its presence data can be several completions old. Asking anyway is
    // where the residual duplicate transfer came from: pages another request installed during the
    // window, re-requested and thrown away on arrival (`INPROG.md`).
    //
    // Only the head, because that is where they are: the arrivals are the earlier request's range,
    // which is a prefix of this one. A hole in the middle would need this range to become two, and
    // nothing measured asks for that.
    //
    // Stopping at the first absent page makes this one probe in the common case, and the cap bounds
    // the worst one: `ObjectControlCmd::Preload` asks for a whole object in a single range without
    // going through the builder, so on a mostly-resident object an uncapped scan would walk
    // `MAX_SIZE / PAGE_SIZE` entries before finding a gap. A region is the right bound because a
    // region is the most staleness ever observed -- rounds 2 and 3 of `dupfix` trimmed exactly 512.
    {
        let scan_limit = len.min(PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE);
        let mut present = 0;
        while present < scan_limit
            && !tree.is_empty_at_level(page.offset(present).as_byte_offset() as u64, 0)
        {
            present += 1;
        }
        if present > 0 {
            profile::PAGER_PROFILE.asked_for_present(present);
        }
        if speculative {
            profile::PAGER_PROFILE.speculative(len, present >= len);
        }
        if present >= len {
            // All of it arrived while we were on our way here, so there is nothing to ask for.
            // The caller's own pages are among them, so it has nothing to wait on either.
            return Ok(tree);
        }
        page = page.offset(present);
        len -= present;
    }
    if !crate::pager::inflight::pager_ready() {
        return Err(ResourceError::Unavailable.into());
    }
    // One hold covers `page_data_request` and `add_request`: both are keyed by this object, so
    // they land on the same shard and stay as atomic as they were under the single lock.
    let mut mgr = lock_inflight_for_obj(obj.id());
    log::trace!(
        "{}: getting page {} from {}",
        current_thread_ref().unwrap().id(),
        page,
        obj.id()
    );
    // Narrowed against what is already in flight before it is keyed, since the range *is* the key.
    // Only the speculative part of the range can be given up; `required` is what the caller blocks
    // on and is never trimmed away.
    let rk = mgr.page_data_request(
        obj.id(),
        page.num(),
        len,
        flags,
        required.map(|(p, l)| (p.num(), l)),
    );
    let slot_gen = crate::pager::inflight::slot_gen();
    let Ok(inflight) = mgr.add_request(rk) else {
        // Speculation that cannot get a slot is not worth waiting for one: the slots it would spin
        // on belong to demand faults, and nothing is waiting on this request. Drop it.
        if speculative {
            drop(mgr);
            return Ok(tree);
        }
        log::warn!("out of pager request slots");
        drop(mgr);
        // The page-table guard cannot be held across this sleep: the completion that frees a
        // slot may need this object's page tables to install pages first. The entry scan on the
        // retry re-checks presence, so anything installed meanwhile is dropped from the ask.
        drop(tree);
        crate::pager::inflight::wait_for_slot(slot_gen);
        let tree = obj.lock_page_tables();
        return submit_page_request(
            obj,
            page,
            len,
            flags,
            speculative,
            tree,
            used_pager,
            required,
            out,
        );
    };
    drop(mgr);
    drop(tree);
    // TODO: more granularity?
    *used_pager = true;
    let mut submitted = false;
    inflight.for_each_pager_req(required.map(|(p, l)| (p.num(), l)), |pager_req| {
        submitted = true;
        queues::submit_pager_request(pager_req, Some(obj), inflight.rk().clone());
    });
    let _ = submitted;
    // Handed back rather than waited on here. See [wait_for_page_requests].
    let _ = out.push(inflight);
    return Ok(obj.lock_page_tables());
}

/// Cap on ranges submitted before waiting. Matches the `reqs` vector `ensure_in_core_pager` fills,
/// which flushes at 16.
const MAX_REQ_BATCH: usize = 16;

/// How many times [`wait_for_page_requests`] re-checks its pages after `setup_wait` declines
/// before handing the fault back to be retried. Small on purpose: this absorbs the race where the
/// request completes between submit and park, and nothing else. A range that was answered without
/// our pages needs a fresh request, which only the caller can issue.
const MAX_WAIT_DECLINES: u32 = 8;

/// Wait until the caller's `required` pages are backed, having already submitted every range.
///
/// **The split from [submit_page_request] is the point.** `ensure_in_core` used to call one
/// submit-and-wait per range in sequence, so a fault whose widened region produced several ranges
/// issued the first, slept until it landed, issued the second, slept again. That made a faulting
/// thread a strictly serial pipeline of depth one, and it is why concurrency measured equal to the
/// thread count at every layer below: `pagepar`'s 4 threads gave the pager `max 4 demand in flight`
/// and the object store `max 1 in flight`, against a pool sized for 2x the core count.
///
/// Parking on the range that actually contains `required` rather than on whichever was submitted
/// first: any of them would be correct -- the loop re-checks the pages themselves and re-parks --
/// but waking on an unrelated range's completion just to find our own pages still absent is a
/// wasted round trip, and with several ranges in flight that is now the common case rather than an
/// impossible one.
fn wait_for_page_requests(
    obj: &ObjectRef,
    inflights: &[Inflight],
    required: Option<(PageNumber, usize)>,
) {
    if inflights.is_empty() {
        return;
    }
    // The one covering the caller's pages, else the first: with no required range the old
    // behaviour is to wait for the whole thing, and any of them serves to park on.
    let target = required
        .and_then(|(rp, rlen)| {
            inflights.iter().find(|i| match i.rk() {
                request::ReqKind::PageData(_, s, l, _) => {
                    *s <= rp.num() && s + l >= rp.num() + rlen
                }
                _ => false,
            })
        })
        .unwrap_or(&inflights[0]);

    // Wait for the pages the caller actually asked for, not for the whole request.
    //
    // `ensure_in_core_pager` widens a one-page touch to an entire large-page region -- 1024
    // pages is the shape that reaches the pager on a first touch (`pagerperf.md` 11) -- and the
    // request completes as a unit, so a thread that needed one page slept for four megabytes of
    // transfer. `required` is what that thread asked for before the widening; once it is
    // backed, the rest of the request is nobody's critical path and finishes behind us.
    //
    // Correct against a lost wakeup because the check happens before each `setup_wait`, and
    // `setup_wait` itself declines to park on a request that is already done -- so a completion
    // landing in either gap is seen rather than slept through.
    let mut early = false;
    let mut declines = 0;
    match required {
        Some((rp, rlen)) => loop {
            if pages_present(obj, rp, rlen) {
                // Only an early exit if the request is *still running*. "The required pages are
                // present" is trivially true once the whole request has completed, so counting
                // that would measure nothing -- and the pager sends one completion per contiguous
                // run, so a whole transfer can arrive as a single batch, in which case waking on
                // it saves nothing at all. This is the difference between the two.
                early = lock_inflight_for(target.rk())
                    .with_request(target.rk(), |r| !r.done())
                    .unwrap_or(false);
                break;
            }
            let mut mgr = lock_inflight_for(target.rk());
            let thread = current_thread_ref().unwrap();
            let Some(guard) = mgr.setup_wait(target, &thread) else {
                // The request we meant to park on is gone: completed, or its slot recycled under
                // us (see `InflightManager::setup_wait`, whose comment says callers "re-check
                // their own condition and come back round" -- this one did not). Returning here
                // sends the caller back through an entire fault, and the refault re-submits a
                // request that can collide the same way: `tcgheavy/round3` churned slot 0 that
                // way 843 times. Re-check the pages instead, since the common reason the request
                // finished is that they landed.
                //
                // Bounded, and a yield rather than a spin: a request that finished without
                // serving our range never will, and only the caller re-submitting can make
                // progress. Falling out after that is the old behaviour, kept as the escape.
                drop(mgr);
                declines += 1;
                if declines >= MAX_WAIT_DECLINES {
                    break;
                }
                schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
                continue;
            };
            drop(mgr);
            crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
            waitstats::park();
            finish_blocking(guard);
        },
        // Nothing narrower to re-check, so every range has to finish: a caller with no required
        // range (preload, COW clone) wants all of what it asked for, and with the submit loop
        // above there are now several requests carrying it rather than one.
        None => {
            for inflight in inflights {
                let mut mgr = lock_inflight_for(inflight.rk());
                let thread = current_thread_ref().unwrap();
                // One park per range, exactly as the old single-range path did: `signal` fires on
                // every batch, so this returns on the first of them rather than at DONE.
                if let Some(guard) = mgr.setup_wait(inflight, &thread) {
                    drop(mgr);
                    crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
                    waitstats::park();
                    finish_blocking(guard);
                }
            }
        }
    }
    waitstats::finish(early);
}

fn cmd_object(req: ReqKind, obj: Option<&ObjectRef>) {
    if !crate::pager::inflight::pager_ready() {
        return;
    }
    let mut mgr = lock_inflight_for(&req);
    let slot_gen = crate::pager::inflight::slot_gen();
    let inflight = match mgr.add_request(req) {
        Ok(x) => x,
        Err(rk) => {
            log::warn!("out of pager request slots");
            drop(mgr);
            crate::pager::inflight::wait_for_slot(slot_gen);
            return cmd_object(rk, obj);
        }
    };
    drop(mgr);
    inflight.for_each_pager_req(None, |pager_req| {
        queues::submit_pager_request(pager_req, obj, inflight.rk().clone());
    });

    let mut mgr = lock_inflight_for(inflight.rk());
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
        finish_blocking(guard);
    };
}

pub fn sync_object(obj: &ObjectRef) {
    cmd_object(ReqKind::new_sync(obj.id()), Some(obj));
}

pub fn del_object(id: ObjID) {
    cmd_object(ReqKind::new_del(id), None);
}

/// Deferred backing-store deletes, and the thread that issues them.
///
/// [del_object] blocks on a pager round trip, and its only caller is `Object::drop` -- which runs
/// wherever the last `ObjectRef` goes away, not on any thread that chose to delete anything. One of
/// those places is the pager completion thread: `queues`' idmap holds an `ObjectRef` per
/// outstanding request, so the DONE branch that removes an entry can be the drop that issues the
/// delete. That parks the *only* thread draining completions on a completion it will never get to
/// process: the pager finishes the delete and goes idle, the kernel keeps that request and whatever
/// else was in flight inflight forever, and everything behind the pager stops (`sysbench.md` F7).
/// The same drop also lands under that map's spinlock, where blocking is not merely a stall.
///
/// So the drop only names the object and this thread does the round trip. The delete is
/// consequently no longer ordered against a later create of the same id, which it never was in any
/// useful sense -- the drop already ran an unbounded time after the delete syscall.
struct Deleter {
    queue: Spinlock<Vec<ObjID>>,
    work: CondVar,
}

static DELETER: Once<Deleter> = Once::new();

/// Ask the deleter thread to tell the pager that `id` is gone. Never blocks.
pub fn queue_del_object(id: ObjID) {
    let Some(deleter) = DELETER.poll() else {
        // No pager queues yet, hence no deleter and no pager to tell; `del_object` itself
        // returns immediately while the inflight manager is not ready.
        del_object(id);
        return;
    };
    deleter.queue.lock().push(id);
    deleter.work.signal();
}

pub(super) fn start_deleter() {
    extern "C" fn deleter_entry() {
        let d = DELETER.wait();
        let mut guard = d.queue.lock();
        loop {
            if guard.is_empty() {
                guard = d.work.wait(guard);
                continue;
            }
            let ids = core::mem::take(&mut *guard);
            drop(guard);
            for id in ids {
                del_object(id);
            }
            guard = d.queue.lock();
        }
    }
    DELETER.call_once(|| Deleter {
        queue: Spinlock::new(Vec::new()),
        work: CondVar::new(),
    });
    start_new_kernel(Priority::USER, deleter_entry, 0);
}

pub fn create_object(id: ObjID, create: &ObjectCreate, nonce: u128) -> Result<(), TwzError> {
    cmd_object(ReqKind::new_create(id, create, nonce), None);
    // Taken, not read: one failure fails one create. A pager that is not ready records nothing and
    // still succeeds here, which is the long-standing behaviour for a create with no backing store.
    match take_create_error(id) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

fn do_sync_region(region: &MapRegion, req: ReqKind, wait: bool) {
    // Backpressure: wait for any sync already in flight for this object before submitting
    // another. Every SyncRegion is unique (see `SyncRegionInfo`), so nothing coalesces them, and
    // the fire-and-forget path (`wait == false`, a null sync_info) otherwise lets a tight sync
    // loop grow an unbounded per-object herd -- the pager serializes them per object, the herd
    // eats all NR_REQUESTS slots, and every other pager op starves behind a minutes-long drain
    // (sysbench pager_sync_dirty_page). This bounds each object to two outstanding: the one
    // running and the one being submitted.
    let mut mgr = loop {
        if !crate::pager::inflight::pager_ready() {
            return;
        }
        let mut mgr = lock_inflight_for_obj(region.object().id());
        let Some(prev) = mgr.find_sync_region(region.object().id()) else {
            break mgr;
        };
        let thread = current_thread_ref().unwrap();
        if let Some(guard) = mgr.setup_wait(&prev, &thread) {
            drop(mgr);
            crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
            finish_blocking(guard);
        } else {
            // Declined: the previous sync is done but not yet removed. Give the completion
            // thread a chance to remove it rather than spinning on the lookup.
            drop(mgr);
            schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
        }
    };
    let slot_gen = crate::pager::inflight::slot_gen();
    let inflight = match mgr.add_request(req) {
        Ok(x) => x,
        Err(rk) => {
            log::warn!("out of pager request slots");
            drop(mgr);
            crate::pager::inflight::wait_for_slot(slot_gen);
            return do_sync_region(region, rk, wait);
        }
    };

    drop(mgr);
    inflight.for_each_pager_req(None, |pager_req| {
        queues::submit_pager_request(pager_req, Some(&region.object()), inflight.rk().clone());
    });

    if !wait {
        return;
    }

    let mut mgr = lock_inflight_for(inflight.rk());
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
        finish_blocking(guard);
    };
}

pub fn sync_region(
    region: &MapRegion,
    dirty: DirtyList,
    sync_info: Option<sync_info>,
    version: u64,
    wait: bool,
) {
    // Writing these pages is what extends the store, so this is where the kernel learns its own new
    // length -- no reading of `MEXT_SIZED`, and nothing to ask the pager. Recorded before the write
    // is confirmed, on purpose: overstating the length only costs a round trip for a page that
    // could have been zero-filled, while understating it would let a later fault serve zeros over
    // data that did reach the disk.
    //
    // Meta pages are skipped. One lives at the top of the object, so letting it into the maximum
    // would declare the whole 1 GB address range backed and silently disable the fast path above.
    if let Some(end) = dirty
        .pages()
        .iter()
        .filter(|(pn, _, _)| !pn.is_meta())
        .map(|(pn, _, count)| pn.offset(*count).as_byte_offset())
        .max()
    {
        region.object().extend_known_len(end as u64);
    }
    let req = ReqKind::new_sync_region(region.object(), dirty, sync_info, version);
    do_sync_region(region, req, wait);
}

/// Bring `reqs` into core, blocking until `required` is backed.
///
/// `required` is the range the caller actually needs, which is generally a small part of `reqs`:
/// the fault path widens a touch to a whole large-page region before getting here. Passing `None`
/// waits for every request in full, which is what a caller with no smaller need (a preload, a COW
/// clone) wants.
pub fn ensure_in_core<'a>(
    obj: &'a ObjectRef,
    mut guard: PtGuard<'a>,
    reqs: &[(PageNumber, usize)],
    flags: PagerFlags,
    speculative: bool,
    used_pager: &mut bool,
    required: Option<(PageNumber, usize)>,
) -> Result<PtGuard<'a>, TwzError> {
    if !obj.use_pager() {
        log::warn!(
            "ensure_in_core called on object {} that does not use a pager",
            obj.id()
        );
        return Ok(guard);
    }

    let total_pages = reqs.iter().fold(0, |acc, x| acc + x.1);

    let avail_pager_mem = crate::memory::tracker::get_outstanding_pager_pages();
    let needed_additional = DEFAULT_PAGER_OUTSTANDING_FRAMES
        .saturating_sub(avail_pager_mem.saturating_sub(total_pages));
    let wait_for_additional =
        avail_pager_mem.saturating_sub(total_pages) < DEFAULT_PAGER_OUTSTANDING_FRAMES / 2;
    let low_mem = crate::memory::tracker::is_low_mem();

    log::debug!(
        "ensure in core {}: {:?}, {} pages (avail = {}, needed = {}, wait = {}, is_low_mem = {})",
        obj.id(),
        reqs,
        total_pages,
        avail_pager_mem,
        needed_additional,
        wait_for_additional,
        low_mem,
    );

    // Keyed on `speculative`, not on the flag: what makes it right to skip under memory pressure,
    // and wrong to block donating memory for, is that nobody is waiting -- not how the pager will
    // route it.
    if speculative && low_mem {
        return Ok(guard);
    }

    if needed_additional > DEFAULT_PAGER_OUTSTANDING_FRAMES / 8 && !low_mem {
        drop(guard);
        // Never block a thread donating memory on behalf of speculation. The caller here is on its
        // way to map the object, not to read these pages; making it wait for the pager to ack a
        // donation puts a real syscall behind work nobody has asked for.
        request_pager_memory(
            needed_additional.min(512),
            wait_for_additional && !speculative,
        );
        guard = obj.lock_page_tables();
    }

    // Every range onto the queue before parking on any of them. The wait is what used to sit
    // inside this loop, and moving it out is the whole change: see [wait_for_page_requests].
    let mut inflights = heapless::Vec::<Inflight, MAX_REQ_BATCH>::new();
    for (req_page, req_len) in reqs {
        guard = submit_page_request(
            obj,
            *req_page,
            *req_len,
            flags,
            speculative,
            guard,
            used_pager,
            required,
            &mut inflights,
        )?;
        if inflights.is_full() {
            break;
        }
    }

    if speculative {
        return Ok(guard);
    }
    // `pages_present` and `setup_wait` both take locks this holds, and the thread is about to
    // sleep -- holding the object's page tables across that is what `get_pages_and_wait` dropped
    // them for.
    drop(guard);
    wait_for_page_requests(obj, &inflights, required);

    // Only when the pages the caller blocked on are still missing: an error against some other
    // part of a widened request says nothing about whether this thread can proceed.
    // TODO: remove this.
    if let Some((rp, rlen)) = required {
        if !inflights.is_empty() && !pages_present(obj, rp, rlen) {
            if let Some(err) = take_page_in_error(obj.id()) {
                log::warn!(
                    "pager failed to supply pages {}..{} of object {}: {}",
                    rp,
                    rp.offset(rlen),
                    obj.id(),
                    err
                );
                return Err(err);
            }
        }
    }

    Ok(obj.lock_page_tables())
}

fn get_memory_for_pager(min_frames: usize) -> Vec<PhysRange> {
    let mut ranges = Vec::new();
    let mut count = 0;
    let outstanding = crate::memory::tracker::get_outstanding_pager_pages();
    if outstanding + min_frames >= MAX_PAGER_OUTSTANDING_FRAMES {
        // Loud, not silent: this refusal is a hang sentence for a pager that is out of memory
        // and cannot evict (pagerwedge.md §3.7) -- an unexplained quiet round with an OOM'd
        // pager is exactly this line not existing.
        log::warn!(
            "pager memory donation refused at cap: {} outstanding + {} asked >= {}",
            outstanding,
            min_frames,
            MAX_PAGER_OUTSTANDING_FRAMES
        );
        return Vec::new();
    }
    while count < min_frames {
        let req_max = (min_frames - count).min(DEFAULT_PAGER_OUTSTANDING_FRAMES);
        let level = if req_max * PHYS_LEVEL_LAYOUTS[0].size() >= PHYS_LEVEL_LAYOUTS[1].size() {
            1
        } else {
            0
        };

        if let Some((frame, len)) = crate::memory::tracker::try_alloc_split_frames(
            FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[level],
        ) {
            for i in 0..len / PHYS_LEVEL_LAYOUTS[0].size() {
                let frame = get_frame(
                    frame
                        .start_address()
                        .offset(i * PHYS_LEVEL_LAYOUTS[0].size())
                        .unwrap(),
                )
                .unwrap();
                frame.inc_refcount();
                assert!(!frame.is_cow());
                assert!(!frame.is_pt());
                assert_eq!(frame.refcount(), 1);
                assert!(frame.size() == PHYS_LEVEL_LAYOUTS[0].size());
                assert!(!frame.is_kernel());
                // it is zeroed because we requested a zeroed frame, but we don't track updates from
                // the pager.
                assert!(!frame.is_zeroed());
            }
            let thiscount = len / PHYS_LEVEL_LAYOUTS[0].size();
            count += thiscount;
            crate::memory::tracker::track_page_pager(thiscount);
            ranges.push(PhysRange::new(
                frame.start_address().raw(),
                frame.start_address().offset(len).unwrap().raw(),
            ));
        } else if let Some(frame) =
            crate::memory::tracker::try_alloc_frame(FrameAllocFlags::ZEROED, PHYS_LEVEL_LAYOUTS[0])
        {
            frame.inc_refcount();
            assert!(!frame.is_cow());
            assert!(!frame.is_pt());
            assert!(frame.refcount() == 1);
            assert!(frame.size() == PHYS_LEVEL_LAYOUTS[0].size());
            assert!(!frame.is_kernel());
            assert!(!frame.is_zeroed());
            count += 1;
            crate::memory::tracker::track_page_pager(1);
            ranges.push(PhysRange::new(
                frame.start_address().raw(),
                frame.start_address().offset(frame.size()).unwrap().raw(),
            ));
        } else {
            // Neither allocator can satisfy us right now. Donate what we have: retrying here
            // advances nothing, and since neither allocator logs, spinning produces a completely
            // silent hang -- the Ready handler never builds its CompletionToPager and the pager
            // handshake never finishes.
            log::warn!(
                "no frames available to donate to pager: got {} of {} requested",
                count,
                min_frames
            );
            break;
        }
    }
    ranges.sort_unstable_by_key(|r| r.start);
    ranges
        .into_iter()
        .coalesce(|a, b| {
            if a.end == b.start {
                Ok(PhysRange {
                    start: a.start,
                    end: b.end,
                })
            } else {
                Err((a, b))
            }
        })
        .collect()
}

struct MemoryProvider {
    /// Pages wanted. `fetch_max`, not a sum: every ask is "top the pager up to this much", so
    /// concurrent askers are naming the same need, not adding debts.
    requested: AtomicUsize,
    /// Completed-provision generation, and the lock both condvars pair with -- holding it across
    /// the provider's check-then-wait is what makes a requester's max-then-signal unlosable.
    generation: Spinlock<u64>,
    work: CondVar,
    done: CondVar,
}

static MEMORY_PROVIDER: Once<MemoryProvider> = Once::new();

/// Ask the provider thread to top up the pager's donated memory, optionally waiting until a
/// provision completes.
///
/// This exists instead of calling [provide_pager_memory] directly for two reasons. The handler
/// threads must never block donating: provide can sleep for a request slot and block on queue
/// space, and the completion handler is the only thread that frees either -- waiting for them
/// there deadlocks the whole pager once a sync storm fills both (sysbench-syncwedge). And
/// concurrent askers used to each send their own donation batch, burning a request slot per
/// range; combining the asks into one atomic and letting the single provider thread send them
/// bounds outstanding memory requests to one batch in flight.
///
/// The wait is advisory backpressure, not a promise of frames: a waiter is released when the
/// next provision completes, which may be one that was already being built when it asked.
pub(super) fn request_pager_memory(min_frames: usize, wait: bool) {
    let Some(p) = MEMORY_PROVIDER.poll() else {
        return;
    };
    p.requested.fetch_max(min_frames, Ordering::SeqCst);
    let mut genr = p.generation.lock();
    p.work.signal();
    if wait {
        let start = *genr;
        while *genr == start {
            genr = p.done.wait(genr);
        }
    }
}

pub(super) fn start_memory_provider() {
    extern "C" fn provider_entry() {
        let p = MEMORY_PROVIDER.wait();
        let mut genr = p.generation.lock();
        loop {
            let n = p.requested.swap(0, Ordering::SeqCst);
            if n == 0 {
                genr = p.work.wait(genr);
                continue;
            }
            drop(genr);
            // wait=true is the single-flight bound: the next batch is not built until the pager
            // has acked this one.
            provide_pager_memory(n, true);
            genr = p.generation.lock();
            *genr += 1;
            p.done.signal();
        }
    }
    MEMORY_PROVIDER.call_once(|| MemoryProvider {
        requested: AtomicUsize::new(0),
        generation: Spinlock::new(0),
        work: CondVar::new(),
        done: CondVar::new(),
    });
    start_new_kernel(Priority::USER, provider_entry, 0);
}

pub fn provide_pager_memory(min_frames: usize, wait: bool) {
    if !crate::pager::inflight::pager_ready() {
        return;
    }
    //print_tracker_stats();
    let ranges = get_memory_for_pager(min_frames);
    log::trace!(
        "allocated {} ranges for pager (min_frames = {}, total = {} KB)",
        ranges.len(),
        min_frames,
        ranges.iter().fold(0, |acc, x| acc + x.len()) / 1024
    );
    //print_tracker_stats();

    // Submit each donation as soon as it has a slot, and when the budget fills wait for this
    // call's own oldest ack rather than sleeping. Collecting the whole batch before submitting
    // any of it is what wedged: a donation shattered by fragmentation into more ranges than
    // there were slots (256 ranges carrying 302 pages of a 16384-frame ask) held every slot
    // with nothing submitted, so no ack could ever free one (spawnbench.md §23).
    let mut inflights = Vec::with_capacity(ranges.len());
    let mut budget_waits = 0usize;
    for range in ranges.iter() {
        let req = ReqKind::new_pager_memory(*range);
        let inflight = loop {
            let mut mgr = lock_shard(shard_idx(None));
            let slot_gen = crate::pager::inflight::slot_gen();
            match mgr.add_request(req.clone()) {
                Ok(inflight) => break inflight,
                Err(_) => {
                    drop(mgr);
                    budget_waits += 1;
                    if !wait_oldest_donation(&mut inflights) {
                        // Nothing of ours outstanding, so the budget is held elsewhere. There is
                        // only one provider thread today; fallback, not a path. A Pages free
                        // bumps the same generation, so this wakes on the next ack.
                        crate::pager::inflight::wait_for_slot(slot_gen);
                    }
                }
            }
        };
        inflight.for_each_pager_req(None, |pager_req| {
            log::trace!("providing: {:?}", pager_req);
            queues::submit_pager_request(pager_req, None, inflight.rk().clone());
        });
        inflights.push(inflight);
    }
    if budget_waits > 0 {
        // Positive control for the wedge fix: this is exactly the shape that used to deadlock.
        // placed/asked makes "the full donation went out" a number rather than an absence -- a
        // shortfall here with no "no frames available" warning would mean the fix shrinks
        // donations, which passing rounds alone cannot reveal.
        let placed: u64 = ranges.iter().map(|r| r.len() as u64).sum();
        log::info!(
            "pager memory donation cycled its budget: {} waits over {} ranges, placed {} of {} asked frames",
            budget_waits,
            ranges.len(),
            placed / 0x1000,
            min_frames
        );
    }

    if wait {
        for inflight in &inflights {
            let mut mgr = lock_inflight_for(inflight.rk());
            let thread = current_thread_ref().unwrap();
            if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
                drop(mgr);
                crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
                finish_blocking(guard);
            };
        }
    }
}

/// Block until the oldest still-outstanding donation from this provide call is acked, freeing
/// budget for the next one. Returns false when there is nothing of ours to wait on.
fn wait_oldest_donation(inflights: &mut Vec<Inflight>) -> bool {
    if inflights.is_empty() {
        return false;
    }
    let inflight = inflights.remove(0);
    let mut mgr = lock_inflight_for(inflight.rk());
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        crate::thread::locktrack::warn_if_blocking_with_mutexes("pager request");
        finish_blocking(guard);
    }
    true
}
