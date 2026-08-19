use alloc::sync::Arc;
use core::time::Duration;

use heapless::index_map::FnvIndexMap;
use twizzler_abi::{
    device::CacheType,
    meta::{MEXT_SIZED, MetaExt, MetaFlags, MetaInfo},
    object::{ObjID, Protections},
    pager::{
        CompletionToKernel, CompletionToPager, KernelCommand, KernelCompletionFlags,
        ObjectEvictFlags, ObjectInfo, ObjectInfoFlags, ObjectRange, PageFlags, PagerCompletionData,
        PagerRequest, PhysRange, RequestFromKernel, RequestFromPager,
    },
    syscall::{MapFlags, NANOS_PER_SEC},
};
use twizzler_rt_abi::{
    error::{ObjectError, RawTwzError, TwzError},
    object::Nonce,
};

use super::{
    DEFAULT_PAGER_OUTSTANDING_FRAMES, inflight::NR_REQUESTS, inflight_mgr, request::ReqKind,
    request_pager_memory,
};
use crate::{
    arch::{PhysAddr, memory::phys_to_virt},
    idcounter::{IdCounter, SimpleId},
    instant::Instant,
    is_test_mode,
    memory::{
        context::{KernelMemoryContext, ObjectContextInfo, kernel_context},
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, merge_frame},
        pagetables::{ContiguousProvider, MappingCursor, MappingFlags, MappingSettings},
        sim_memory_pressure,
        tracker::{FrameAllocFlags, FrameAllocator, start_reclaim_thread},
    },
    obj::{LookupFlags, Object, ObjectRef, PageNumber, lookup_object},
    once::Once,
    queue::{ManagedQueueReceiver, QueueObject},
    security::KERNEL_SCTX,
    spinlock::Spinlock,
    syscall::sync::sys_thread_sync,
    thread::{
        current_thread_ref,
        entry::{run_closure_in_new_thread, start_new_kernel},
        priority::Priority,
    },
};

#[derive(Clone, Debug)]
struct SentRequestInfo {
    req: RequestFromKernel,
    obj: Option<ObjectRef>,
    reqkind: ReqKind,
}

struct RequestSender {
    ids: IdCounter,
    queue: QueueObject<RequestFromKernel, CompletionToKernel>,
    idmap: Spinlock<heapless::index_map::FnvIndexMap<u32, SentRequestInfo, NR_REQUESTS>>,
}

static SENDER: Once<RequestSender> = Once::new();

static RECEIVER: Once<ManagedQueueReceiver<RequestFromPager, CompletionToPager>> = Once::new();

fn pager_request_copy_user_phys(
    target_object: ObjID,
    offset: usize,
    len: usize,
    phys: PhysRange,
    write_phys: bool,
) -> CompletionToPager {
    log::debug!("copy user phys {:?} {:?}", phys, write_phys);
    let Ok(phys_start) = PhysAddr::new(phys.start) else {
        return CompletionToPager::new(PagerCompletionData::Error(
            TwzError::INVALID_ARGUMENT.into(),
        ));
    };

    let Ok(object) = lookup_object(target_object, LookupFlags::empty()).ok_or(()) else {
        return CompletionToPager::new(PagerCompletionData::Error(
            TwzError::INVALID_ARGUMENT.into(),
        ));
    };
    let ko = kernel_context().insert_kernel_object::<()>(ObjectContextInfo::new(
        object,
        Protections::READ | Protections::WRITE,
        CacheType::WriteBack,
        MapFlags::empty(),
    ));
    let Ok(vaddr) = ko.start_addr().offset(offset) else {
        return CompletionToPager::new(PagerCompletionData::Error(
            TwzError::INVALID_ARGUMENT.into(),
        ));
    };

    let vphys = phys_start.kernel_vaddr();
    log::debug!("addrs: {:?} {:?}", vaddr, vphys);
    let user_slice = unsafe { core::slice::from_raw_parts_mut(vaddr.as_mut_ptr(), len) };
    let phys_slice =
        unsafe { core::slice::from_raw_parts_mut(vphys.as_mut_ptr::<u8>(), phys.len()) };

    let copy_len = core::cmp::min(user_slice.len(), phys_slice.len());
    let (target_slice, source_slice) = if write_phys {
        (phys_slice, user_slice)
    } else {
        (user_slice, phys_slice)
    };
    target_slice[0..copy_len].copy_from_slice(&source_slice[0..copy_len]);
    target_slice[copy_len..].fill(0);

    CompletionToPager::new(PagerCompletionData::Okay)
}

fn pager_register_phys(phys: u64, len: u64) -> Result<(), TwzError> {
    log::debug!("register phys: {:x} - {:x}", phys, phys + len);
    let paddr = PhysAddr::new(phys).map_err(|_| TwzError::INVALID_ARGUMENT)?;
    let vaddr = phys_to_virt(paddr);
    let cursor = MappingCursor::new(vaddr, len as usize);
    let mut fa = FrameAllocator::new(
        FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
        PHYS_LEVEL_LAYOUTS[0],
    );
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::WriteBack,
        MappingFlags::GLOBAL,
    );
    let mut phys = ContiguousProvider::new(paddr, len as usize, settings);
    kernel_context().with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
    Ok(())
}

pub(super) fn pager_request_handler_main() {
    let receiver = RECEIVER.wait();
    loop {
        receiver.handle_request(|_id, req| match req.cmd() {
            PagerRequest::Ready => {
                log::info!("pager ready");
                inflight_mgr().lock().set_ready();
                request_pager_memory(DEFAULT_PAGER_OUTSTANDING_FRAMES, false);

                start_reclaim_thread();
                crate::obj::start_reaper_thread();
                // TODO
                if is_test_mode() && false {
                    run_closure_in_new_thread(Priority::USER, || {
                        sim_memory_pressure();
                    });
                }

                CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Okay)
            }
            PagerRequest::CopyUserPhys {
                target_object,
                offset,
                len,
                phys,
                write_phys,
            } => pager_request_copy_user_phys(target_object, offset, len, phys, write_phys),
            PagerRequest::RegisterPhys(phys, len) => match pager_register_phys(phys, len) {
                Ok(_) => CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Okay),
                Err(e) => CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Error(
                    RawTwzError::new(e.raw()),
                )),
            },
        });
    }
}

/// Release the reference the pager handed us for a frame.
///
/// Dropping to zero means nothing took the frame: `add_frame_if_absent` declined because the object
/// already had that page, which is what two overlapping in-flight requests produce by construction
/// (`add_request` coalesces on an exact `ReqKind`, so a request overlapping another never compares
/// equal to it -- `pagerperf.md` 18). The waste is counted in [`super::profile`], where it can be
/// read against what was delivered rather than on its own.

/// Why a large-page merge does or does not happen.
///
/// Nothing has ever counted this, and the whole "splitting a request costs a large page" tradeoff
/// rests on merges actually occurring. A merge needs the object page *and* the physical address to
/// be 2 MiB-aligned at the same point in a run, and the pager's donations are aligned to their own
/// chunks rather than to the object offsets they land on -- so `phys_ok` well below `candidates` is
/// the signal that the tradeoff is imaginary and requests can be split freely.
mod largepage {
    use core::sync::atomic::{AtomicU64, Ordering};

    static CANDIDATES: AtomicU64 = AtomicU64::new(0);
    static PHYS_OK: AtomicU64 = AtomicU64::new(0);
    static MERGED: AtomicU64 = AtomicU64::new(0);

    /// Called once per point in a completion where the object side would allow a merge.
    pub fn record(phys_aligned: bool, merged: bool) {
        if phys_aligned {
            PHYS_OK.fetch_add(1, Ordering::Relaxed);
        }
        if merged {
            MERGED.fetch_add(1, Ordering::Relaxed);
        }
        let n = CANDIDATES.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            log::info!(
                "LARGEPAGE: {} object-aligned candidates, {} also phys-aligned, {} merged",
                n,
                PHYS_OK.load(Ordering::Relaxed),
                MERGED.load(Ordering::Relaxed),
            );
        }
    }
}

/// A/B knob for the batched install. `1` restores the per-page path this replaced -- one lock, one
/// presence walk, one mapping walk, one precharge and one TLB invalidation round *per page* -- so
/// what batching bought can be measured without reverting anything.
const MAX_INSTALL_RUN: usize = 64;

/// Pages from `pn` to the next 2 MiB object boundary, at least one.
fn pages_to_large_boundary(pn: PageNumber) -> usize {
    let off = pn.as_byte_offset();
    let large = PHYS_LEVEL_LAYOUTS[1].size();
    ((off + 1).next_multiple_of(large) - off) / PageNumber::PAGE_SIZE
}

fn page_at(base: PhysAddr, i: usize) -> PhysAddr {
    base.offset(i * PageNumber::PAGE_SIZE).unwrap()
}

fn release_pager_frame(frame: FrameRef) {
    if frame.dec_refcount() == 0 {
        crate::memory::tracker::free_frame(frame);
    }
}

fn pager_compl_handle_page_data(
    request: &SentRequestInfo,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    flags: PageFlags,
) {
    let handle_start = Instant::now();
    let pcount = phys_range.page_count();
    log::debug!(
        "got : {} {:?} {:?} ({} pages)",
        request.obj.as_ref().unwrap().id(),
        obj_range,
        phys_range,
        pcount
    );
    if obj_range.len() != phys_range.len() {
        log::warn!(
            "object and phys range lengths differ (obj: {}, phys: {})",
            obj_range.len(),
            phys_range.len()
        );
    }

    if !flags.contains(PageFlags::WIRED) {
        log::trace!(
            "untrack {:?} from pager memory ({} pages, pager has {} pages left)",
            phys_range,
            pcount,
            crate::memory::tracker::get_outstanding_pager_pages()
        );
        crate::memory::tracker::untrack_page_pager(pcount);
        if crate::memory::tracker::get_outstanding_pager_pages()
            < DEFAULT_PAGER_OUTSTANDING_FRAMES / 2
        {
            request_pager_memory(DEFAULT_PAGER_OUTSTANDING_FRAMES, false);
        }
    }

    let mut count = 0;
    let mut installed = 0;
    let mut dup = 0;
    let mut dup_large = 0;
    let mut merged = 0;
    let max_obj = obj_range.page_count();
    let max_phys = phys_range.page_count();
    let max = max_obj.min(max_phys);
    let pages_per_large = PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE;
    while count < max {
        let objpage_nr = obj_range.pages().nth(count).unwrap();
        let physpage_nr = phys_range.pages().nth(count).unwrap();

        let pn = PageNumber::from(objpage_nr as usize);
        let pa = PhysAddr::new(physpage_nr * PageNumber::PAGE_SIZE as u64).unwrap();

        let thiscount = (max_obj - count).min(max_phys - count);

        log::trace!(
            "==> {} {} {} ({:x} {} ({})) {} {}",
            pa.is_aligned_to(PHYS_LEVEL_LAYOUTS[1].size()),
            pn.as_byte_offset()
                .is_multiple_of(PHYS_LEVEL_LAYOUTS[1].size()),
            thiscount,
            pa.raw(),
            pn.num(),
            pn.num() % 512,
            max_obj,
            max_phys
        );

        // Split out so the reasons a merge fails can be counted separately; evaluated in cost
        // order, so the page-table lock is only taken once everything cheap has passed.
        let candidate = pn
            .as_byte_offset()
            .is_multiple_of(PHYS_LEVEL_LAYOUTS[1].size())
            && thiscount >= pages_per_large
            && !flags.contains(PageFlags::WIRED);
        let phys_aligned = candidate && pa.is_aligned_to(PHYS_LEVEL_LAYOUTS[1].size());
        // The region must still be empty. Mapping a large page over a level-1 entry that has become
        // a table -- because some page inside it is already present -- does not merge: `Table::map`
        // overwrites the entry, orphaning the table below it and leaving the frame it held mapped
        // in whoever already had it, now pointing at different physical memory.
        // `ensure_in_core_pager` checks this before *asking* for a large run, but a page
        // can arrive between the ask and this completion, and serving a required subrange
        // first makes partially-populated regions ordinary rather than rare.
        let can_merge = phys_aligned
            && request
                .obj
                .as_ref()
                .unwrap()
                .lock_page_tables()
                .is_empty_at_level(pn.as_byte_offset() as u64, 1);
        if candidate {
            largepage::record(phys_aligned, can_merge);
        }
        let thiscount = if can_merge {
            let frame = crate::memory::frame::get_frame(pa).unwrap();
            assert!(!frame.is_pt());
            assert_eq!(frame.refcount(), 1);
            assert!(!frame.is_cow());
            assert!(frame.size() == PHYS_LEVEL_LAYOUTS[0].size());
            let frame = merge_frame(frame);
            assert!(!frame.is_pt());
            assert_eq!(frame.refcount(), 1);
            assert!(!frame.is_cow());
            assert!(frame.size() == PHYS_LEVEL_LAYOUTS[1].size());
            request.obj.as_ref().unwrap().add_frame(pn, frame);
            release_pager_frame(frame);
            installed += pages_per_large;
            merged += 1;
            pages_per_large
        } else if flags.contains(PageFlags::WIRED) {
            request
                .obj
                .as_ref()
                .unwrap()
                .map_phys(
                    pn.as_byte_offset(),
                    pa,
                    pa.offset(PageNumber::PAGE_SIZE).unwrap(),
                    CacheType::WriteBack,
                )
                .unwrap();
            installed += 1;
            1
        } else {
            // Everything up to the next 2 MiB object boundary, in one call. A merge can only
            // *start* at such a boundary, so stopping there gives up no merge the per-page loop
            // would have found -- and the run below it is exactly what `add_frames_if_absent`
            // wants: contiguous in both the object and physical memory, and charged once rather
            // than 130 times.
            let run = pages_to_large_boundary(pn)
                .min(max - count)
                .min(MAX_INSTALL_RUN);
            for i in 0..run {
                let frame = crate::memory::frame::get_frame(page_at(pa, i)).unwrap();
                assert!(!frame.is_pt());
                assert_eq!(frame.refcount(), 1);
                assert!(!frame.is_cow());
            }
            let tally = request
                .obj
                .as_ref()
                .unwrap()
                .add_frames_if_absent(pn, pa, run);
            installed += tally.installed;
            dup += tally.dup;
            dup_large += tally.dup_large;
            // Unconditionally, exactly as when this was per page: this drops the reference the
            // pager handed us, and for a page that was already present -- which `Table::map`
            // skipped without referencing -- that drop is what frees it.
            for i in 0..run {
                release_pager_frame(crate::memory::frame::get_frame(page_at(pa, i)).unwrap());
            }
            run
        };
        count += thiscount;
    }

    super::profile::PAGER_PROFILE.completion(max, installed, dup, dup_large, merged);
    super::profile::PAGER_PROFILE.installed_ns((Instant::now() - handle_start).as_nanos() as u64);

    let mut mgr = inflight_mgr().lock();
    if dup_large > 0 {
        // Only the large-page case, which is at most a line or two a boot -- logging every
        // completion hid it (see the note in `get_pages_and_wait`). The counters carry everything
        // else.
        let id = request.obj.as_ref().unwrap().id();
        log::info!(
            "DUPSRC land: obj {} completion {:?} -- {} delivered, {} installed, {} dup ({} under a \
             large page) -- served {:?}; page-data still in flight for it: {:?}",
            id,
            obj_range,
            max,
            installed,
            dup,
            dup_large,
            request.reqkind,
            mgr.page_data_ranges(id),
        );
    }
    mgr.with_request(&request.reqkind, |req| {
        req.mark_first_completion();
        if req.finished_pages(count) {
            req.mark_done();
        }
        // Signal on every batch, not only on the last one. A thread blocked here generally needs a
        // small part of what it is waiting for -- the fault path widens a one-page touch to a whole
        // large-page region -- so waking it now lets it re-check its own pages and go, with the
        // rest of the transfer landing behind it. A waiter whose pages have not arrived re-parks,
        // which costs it one pass round `get_pages_and_wait`'s loop.
        req.signal();
    });
}

/// Take physical pages the pager filled with `obj`'s meta page and install them.
///
/// The meta page is the object's last page, and `check_id` reads it on the first map of every
/// object -- so without this it is a page-data round trip billed to the mapping path
/// (`mapperf.md`: 49% of `insert_object`). Unlike the page-data path this needs no large-page
/// branch: it is one page, and the last one, so it can never start a large-aligned run.
fn install_meta_page(obj: &ObjectRef, phys_range: PhysRange) {
    let pcount = phys_range.page_count();
    if pcount == 0 {
        log::warn!("pager sent an empty meta page for {}", obj.id());
        return;
    }
    if pcount != 1 {
        log::warn!(
            "pager sent {} pages for {}'s meta page, using the first",
            pcount,
            obj.id()
        );
    }
    crate::memory::tracker::untrack_page_pager(pcount);
    let Some(pa) = phys_range
        .pages()
        .next()
        .and_then(|p| PhysAddr::new(p * PageNumber::PAGE_SIZE as u64).ok())
    else {
        log::warn!("pager sent an unusable meta page {:?}", phys_range);
        return;
    };
    let Some(frame) = crate::memory::frame::get_frame(pa) else {
        log::warn!(
            "pager sent a meta page outside of physical memory: {:?}",
            pa
        );
        return;
    };
    assert!(!frame.is_pt());
    assert!(!frame.is_cow());
    obj.add_frame(PageNumber::meta_page(), frame);
    if frame.dec_refcount() == 0 {
        crate::memory::tracker::free_frame(frame);
    }
    if crate::memory::tracker::get_outstanding_pager_pages() < DEFAULT_PAGER_OUTSTANDING_FRAMES / 2
    {
        request_pager_memory(DEFAULT_PAGER_OUTSTANDING_FRAMES, false);
    }
}

/// Build an object's meta page from the fields the pager sent, instead of moving a page across.
///
/// For an external file there is nothing to move: the pager invents that metadata from the file's
/// length, so the length is the only thing it actually has to send. Building it here costs one
/// zeroed frame and ~64 bytes of writes, against either a `CopyUserPhys` on the single-outstanding
/// pager->kernel channel (`pagerperf.md` 5) or a later fault when userspace reads `MEXT_SIZED`.
/// Same construction `initrd.rs` does for boot objects.
///
/// `write_bytes` is deliberately not used: it goes through `ensure_in_core` for any pager-backed
/// object, which would issue the very page-in this exists to avoid -- from the pager completion
/// thread, at that.
fn synthesize_meta_page(obj: &ObjectRef, info: &ObjectInfo) {
    let Some(frame) =
        crate::memory::tracker::try_alloc_frame(FrameAllocFlags::ZEROED, PHYS_LEVEL_LAYOUTS[0])
    else {
        // Not fatal: without a meta page the first `check_id` reads it the old way.
        log::warn!(
            "no frame available to synthesize a meta page for {}",
            obj.id()
        );
        return;
    };
    let meta = MetaInfo {
        nonce: Nonce(info.nonce),
        kuid: info.kuid,
        flags: MetaFlags::empty(),
        default_prot: info.def_prot,
        fotcount: 0,
        extcount: 1,
    };
    let ext = MetaExt::new(MEXT_SIZED, info.size);
    // Safety: the frame is freshly allocated, zeroed, and a whole page; both writes land inside it.
    // Unaligned because the extension follows `MetaInfo` at its natural end, not at its alignment.
    unsafe {
        let base = frame.virtaddr().as_mut_ptr::<u8>();
        base.cast::<MetaInfo>().write_unaligned(meta);
        base.add(size_of::<MetaInfo>())
            .cast::<MetaExt>()
            .write_unaligned(ext);
    }
    // No refcount dance, unlike `install_meta_page`: a fresh frame arrives at zero and `map_page`
    // takes the reference that makes the object its owner.
    obj.add_frame(PageNumber::meta_page(), frame);
}

fn pager_compl_handle_object_info(id: ObjID, info: ObjectInfo, rk: &ReqKind) {
    let handle_start = Instant::now();
    let obj = Arc::new(Object::new(id, info.lifetime, &[]));
    // What the store holds right now -- and only when the pager says so. `size` defaults to zero,
    // which is a legitimate length, so acting on an unflagged value reads "the pager did not fill
    // this in" as "the object is empty" and zero-fills over its contents. Left unset, the object
    // simply keeps asking the pager, which is what it did before.
    if info.flags.contains(ObjectInfoFlags::SIZE_VALID) {
        obj.set_known_len(info.size);
    }
    // Both before `register_object`, so nothing can look the object up and race a `check_id`
    // against either.
    if info.flags.contains(ObjectInfoFlags::META_PAGE) {
        install_meta_page(&obj, info.meta_page);
    } else if info.flags.contains(ObjectInfoFlags::SYNTH_META) {
        synthesize_meta_page(&obj, &info);
    }
    if info.flags.contains(ObjectInfoFlags::VALIDATED) {
        // The pager vouches for the id, so the hash is skipped -- but `default_prot` is a separate
        // question, and `ObjectInfo` only carries it for the backings whose metadata the pager
        // invents. A stored object's is on its meta page, which the branch above has just made
        // resident, so read it from there: this is the memoized read `check_id` would have done
        // anyway, minus the sha256, and taking `info.def_prot` instead would hand every stored
        // object an empty grant.
        // Only when a meta page is actually resident -- the branch above has just installed one.
        // `read_meta` otherwise *populates*, which from inside the pager's own completion handler
        // means asking the pager for a page while servicing its reply.
        let has_meta = info
            .flags
            .intersects(ObjectInfoFlags::META_PAGE | ObjectInfoFlags::SYNTH_META);
        let prot = if has_meta {
            obj.read_meta()
                .map(|meta| meta.default_prot)
                .unwrap_or(info.def_prot)
        } else {
            info.def_prot
        };
        obj.set_verified_id(true, prot);
    }
    crate::obj::register_object(obj);
    inflight_mgr().lock().request_ready(rk);
    // After `request_ready`, so the stamp is the moment the waiter became runnable rather than the
    // moment the completion arrived: what the split is for is separating this whole segment from
    // the wait for a cpu that follows it.
    let now = Instant::now();
    super::profile::lookupstats::info_ready(
        now.into_time_span().as_nanos() as u64,
        (now - handle_start).as_nanos() as u64,
    );
}

fn pager_compl_handle_error(request: RequestFromKernel, err: TwzError, rk: &ReqKind) {
    log::debug!("pager returned error: {} for {:?}", err, request);
    match err {
        TwzError::Object(ObjectError::NoSuchObject) => {
            if let KernelCommand::ObjectInfoReq(obj_id) = request.cmd() {
                crate::obj::no_exist(obj_id);
                inflight_mgr().lock().request_ready(rk);
            }
        }
        _ => {
            log::error!("pager returned error: {} for {:?}", err, request);
            // Before the manager lock, not under it: recorded so the thread this is about to wake
            // can be told why its pages never came.
            if let KernelCommand::PageDataReq(obj_id, ..) = request.cmd() {
                super::record_page_in_error(obj_id, err);
            }
            inflight_mgr().lock().request_ready(rk);
        }
    }
}

pub(super) fn pager_compl_handler_main() {
    let sender = SENDER.wait();

    let mut count = 0;
    let mut elapsed = 0;
    let mut last_ticks: Option<Instant> = None;
    let current_thread = current_thread_ref().unwrap();
    loop {
        let current_ticks = Instant::now();
        assert!(!current_thread.is_critical());
        let completion = sender.queue.recv_completion();
        assert!(!current_thread.is_critical());

        count += 1;

        if let Some(last_ticks) = last_ticks {
            elapsed += (current_ticks - last_ticks).as_nanos() as u64;
        }
        last_ticks = Some(current_ticks);

        if elapsed >= NANOS_PER_SEC {
            log::trace!(
                "pager completion thread processed {} entries over the last {}ms",
                count,
                elapsed / (NANOS_PER_SEC / 1000),
            );
            count = 0;
            elapsed = 0;
        }

        let idmap_start = Instant::now();
        let idmap = sender.idmap.lock();
        super::profile::PAGER_PROFILE.idmap_lock((Instant::now() - idmap_start).as_nanos() as u64);
        let Some(request) = idmap.get(&completion.0).cloned() else {
            drop(idmap);
            logln!("warn -- received completion for unknown request");
            continue;
        };
        // Immediately, as the temporary this used to be did. Everything below takes the
        // inflight-manager mutex, and the submit path takes this spinlock while holding nothing --
        // holding it across that would invert the order.
        drop(idmap);
        assert!(!current_thread.is_critical());
        log::trace!("got completion for {:?}: {:?}", request.req, completion.1);

        match completion.1.data() {
            twizzler_abi::pager::KernelCompletionData::PageDataCompletion(
                _,
                obj_range,
                phys_range,
                flags,
            ) => pager_compl_handle_page_data(&request, obj_range, phys_range, flags),
            twizzler_abi::pager::KernelCompletionData::ObjectInfoCompletion(id, info) => {
                pager_compl_handle_object_info(id, info, &request.reqkind)
            }
            twizzler_abi::pager::KernelCompletionData::Error(err) => {
                pager_compl_handle_error(request.req, err.error(), &request.reqkind)
            }
            _ => {}
        };
        assert!(!current_thread.is_critical());

        if completion.1.flags().contains(KernelCompletionFlags::DONE) {
            let mut mgr = inflight_mgr().lock();
            if let KernelCommand::ObjectEvict(evict) = request.req.cmd() {
                if evict.flags.contains(ObjectEvictFlags::FENCE) {
                    mgr.remove_request(&request.reqkind);
                }
            } else {
                mgr.remove_request(&request.reqkind);
            }
            // Bound, not discarded in the statement: the entry owns an `ObjectRef`, and dropping
            // the last one runs `Object::drop`. That is deferred now (`pager::queue_del_object`),
            // but running any object destructor under this spinlock is still the wrong shape.
            let removed = sender.idmap.lock().remove(&completion.0);
            drop(removed);
            sender.ids.release_simple(SimpleId::from(completion.0));
        }
    }
}

pub fn submit_pager_request(mut req: RequestFromKernel, obj: Option<&ObjectRef>, reqkind: ReqKind) {
    let sender = SENDER.wait();
    let id = sender.ids.next_simple().value() as u32;
    let stamp_key = reqkind.clone();
    let idmap_start = Instant::now();
    let mut idmap = sender.idmap.lock();
    super::profile::PAGER_PROFILE.idmap_lock((Instant::now() - idmap_start).as_nanos() as u64);
    let mut old = idmap.insert(
        id,
        SentRequestInfo {
            req,
            obj: obj.cloned(),
            reqkind,
        },
    );
    // Before sleeping below, and before the submit: this must not be held across either.
    drop(idmap);
    while let Err((id, sri)) = old {
        log::warn!("overflowing pager queue, waiting...");
        let _ = sys_thread_sync(&mut [], Some(&mut Duration::from_millis(200)));
        old = sender.idmap.lock().insert(id, sri);
    }
    if let Ok(Some(ref old)) = old {
        log::warn!(
            "replaced old item on request index ({}: {:?} -> {:?})",
            id,
            old,
            req
        );
    }
    // Stamped here rather than at entry, so the overflow wait above lands in the kernel's own
    // submit segment (`pre_ns`) instead of being billed to the pager as queue transit. Only the
    // wire copy carries it; the `idmap` snapshot taken above is for completion handling, which has
    // no use for it.
    req.set_submit_ns(Instant::now().into_time_span().as_nanos() as u64);
    sender.queue.submit(req, id);
    // After the submit, so the segment covers the enqueue itself, including the overflow wait
    // above. Every caller drops the inflight lock before submitting, so taking it here is safe.
    inflight_mgr()
        .lock()
        .with_request(&stamp_key, |r| r.mark_submitted());
}

extern "C" fn pager_compl_handler_entry() {
    pager_compl_handler_main();
}

extern "C" fn pager_request_handler_entry() {
    pager_request_handler_main();
}

pub fn init_pager_queue(id: ObjID, outgoing: bool) {
    let obj = match lookup_object(id, LookupFlags::empty()) {
        crate::obj::LookupResult::Found(o) => o,
        _ => panic!("pager queue not found"),
    };
    log::debug!(
        "[kernel::pager] registered {} pager queue: {}",
        if outgoing { "sender" } else { "receiver" },
        id
    );
    if outgoing {
        let queue = QueueObject::<RequestFromKernel, CompletionToKernel>::from_object(obj);
        SENDER.call_once(|| RequestSender {
            ids: IdCounter::new(),
            queue,
            idmap: Spinlock::new(FnvIndexMap::new()),
        });
    } else {
        let queue = QueueObject::<RequestFromPager, CompletionToPager>::from_object(obj);
        let receiver = ManagedQueueReceiver::new(queue);
        RECEIVER.call_once(|| receiver);
    }
    if SENDER.poll().is_some() && RECEIVER.poll().is_some() {
        super::start_memory_provider();
        super::start_deleter();
        // TODO: these should be higher?
        start_new_kernel(Priority::REALTIME, pager_compl_handler_entry, 0);
        start_new_kernel(Priority::USER, pager_request_handler_entry, 0);
        log::debug!("pager queues and handlers initialized");
    }
}
