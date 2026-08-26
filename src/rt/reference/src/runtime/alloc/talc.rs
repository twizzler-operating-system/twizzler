//! Primary allocator, for compartment-local allocation. One tricky aspect to this is that we need
//! to support allocation before the runtime is fully ready, so to avoid calling into std, we
//! implement a manual spinlock around the allocator until the better Mutex is available. Once it
//! is, we move the allocator into the mutex, and use that.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
    sync::atomic::Ordering,
};
use std::{alloc::Allocator, mem::size_of, sync::atomic::AtomicUsize};

use secgate::get_sctx_id;
use twizzler_abi::{
    simple_mutex::Mutex,
    syscall::{
        sys_object_add_note, sys_object_ctrl, CreateTieFlags, CreateTieSpec, DeleteFlags,
        ObjectControlCmd,
    },
};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const MIN_ALIGN: usize = 16;

/// Early allocations at or above this size get an `ALLCBIG` record when the child-startup diag
/// switch is armed; sized to catch mlibc's 512KB slab chunks without recording ordinary traffic.
const EARLY_ALLOC_DIAG_MIN: usize = 16 * 1024;

/// Zeroed early allocations at or above this size are served from the virgin region (see
/// [`RuntimeOom::virgin_next`]) instead of talc + memset. Small requests stay on talc: the memset
/// is cheap there, and burning virgin space on them would exhaust the region for the chunks that
/// matter.
const EARLY_ZERO_BUMP_MIN: usize = 16 * 1024;

/// Span tail held back from talc as the virgin region. mlibc's ctors take ~2MB of slab chunks per
/// compartment; 16MB leaves generous headroom, and exhaustion just falls back to talc + memset.
const VIRGIN_RESERVE: usize = 16 * 1024 * 1024;

use talc::{OomHandler, Span, Talc};
use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, sys_object_map, BackingType, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_rt_abi::object::MapFlags;

use super::super::OUR_RUNTIME;
use crate::runtime::RuntimeState;

pub static LOCAL_ALLOCATOR: LocalAllocator = LocalAllocator {
    inner: Mutex::new(LocalAllocatorInner::new()),
    bootstrap_alloc_slot: AtomicUsize::new(0),
};

unsafe impl Sync for LocalAllocator {}

pub struct LocalAllocator {
    inner: Mutex<LocalAllocatorInner>,
    pub(super) bootstrap_alloc_slot: AtomicUsize,
}

impl LocalAllocator {
    pub fn get_id_from_ptr(&self, ptr: *const u8) -> Option<ObjID> {
        let slot = ptr as usize / MAX_SIZE;
        let inner = self.inner.lock();
        inner.talc.oom_handler.objects.iter().find_map(|info| {
            if info.0 == slot {
                Some(info.1)
            } else {
                None
            }
        })
    }

    pub fn is_ptr_early_alloc(&self, ptr: *const u8) -> bool {
        let slot = ptr as usize / MAX_SIZE;
        let inner = self.inner.lock();
        inner
            .early_talc
            .oom_handler
            .objects
            .iter()
            .any(|info| info.0 == slot)
    }

    pub fn freeze_early_allocs(&self) {
        let mut inner = self.inner.lock();
        inner.early_allocs_frozen = true;
    }
}

/// Every heap object *this compartment's* allocator owns, as `[slot, id_hi, id_lo]` triples,
/// followed by `[n_main, n_early]`. Returns words written.
///
/// DIAG, and the point is ownership. `note=heap` is written identically by every compartment's
/// allocator, so a census grower reading `note=heap` says "a heap" and not "whose". Walking the
/// caller's own `oom_handler.objects` answers it exactly: an id in this list belongs to the calling
/// compartment's allocator, and an id absent from it does not -- which a slot-map join cannot say,
/// because a slot map is a snapshot and an object created after it can never match.
#[no_mangle]
pub extern "C-unwind" fn __twz_rt_diag_heap_objects(out: *mut u64, n: usize) -> usize {
    if out.is_null() || n < 2 {
        return 0;
    }
    let inner = LOCAL_ALLOCATOR.inner.lock();
    let mut w = 0usize;
    let mut counts = [0usize; 2];
    for (which, objs) in [
        &inner.talc.oom_handler.objects,
        &inner.early_talc.oom_handler.objects,
    ]
    .into_iter()
    .enumerate()
    {
        for (slot, id) in objs.iter() {
            if w + 3 + 2 > n {
                break;
            }
            let raw = id.raw();
            unsafe {
                *out.add(w) = *slot as u64;
                *out.add(w + 1) = (raw >> 64) as u64;
                *out.add(w + 2) = raw as u64;
            }
            w += 3;
            counts[which] += 1;
        }
    }
    unsafe {
        *out.add(w) = counts[0] as u64;
        *out.add(w + 1) = counts[1] as u64;
    }
    w + 2
}

struct LocalAllocatorInner {
    talc: Talc<RuntimeOom>,
    early_talc: Talc<RuntimeOom>,
    early_allocs_frozen: bool,
}

struct RuntimeOom {
    list_obj: Option<(usize, ObjID)>,
    objects: Vec<(usize, ObjID), FailAlloc>,
    /// Bump cursor/limit of the *virgin region*: the tail of the most recent heap span, held back
    /// from talc at claim time. Talc writes free-list metadata into free memory, so talc-carved
    /// memory is not provably zero — but this range is backed by a fresh zero-filled object and
    /// nothing ever writes it before handout, so large `ZERO_MEMORY` requests served from here can
    /// skip the memset (measured at 109us per 512KB mlibc slab chunk, 4 chunks per spawn;
    /// `spawnbench.md` §31). Zero until the first span claim.
    virgin_next: usize,
    virgin_top: usize,
}

fn release_object(id: ObjID) {
    monitor_api::monitor_rt_object_unmap(id, MapFlags::READ | MapFlags::WRITE).unwrap();
}

fn create_and_map() -> Option<(usize, ObjID)> {
    let is_mon = OUR_RUNTIME.state().contains(RuntimeState::IS_MONITOR);
    let ties = if is_mon {
        &[][..]
    } else {
        let cc = get_sctx_id();
        assert!(
            cc.raw() != 0,
            "cannot create runtime object without a security context"
        );
        &[CreateTieSpec::new(cc, CreateTieFlags::empty()).into()][..]
    };
    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
            Protections::all(),
        ),
        &[],
        ties,
    )
    .ok()?;

    if is_mon {
        // Map directly, avoiding complex machinery in the monitor that depends on an allocator.
        let slot = OUR_RUNTIME.allocate_slot().unwrap();
        sys_object_map(
            None,
            id,
            slot,
            Protections::READ | Protections::WRITE,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .unwrap();
        let _ = sys_object_add_note(id, b"monitor-heap");
        return Some((slot, id));
    }

    if std::env::var("MONDEBUG").is_ok() {
        twizzler_abi::klog_println!("created object {} for allocation", id,)
    }

    let slot = monitor_api::monitor_rt_object_map(id, MapFlags::READ | MapFlags::WRITE).ok();

    let _ = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0)
        .inspect_err(|e| twizzler_abi::klog_println!("failed to delete heap object {}: {}", id, e));
    // `heap:<low 64 bits of the owning security context>`, not bare `heap`.
    //
    // Every compartment's allocator writes this note, so a leak census reporting `note=heap` names
    // a kind and not an owner -- and "whose heap grew" is the whole question when several
    // compartments' heaps are mapped in one address space. Hand-formatted into a stack buffer
    // because this runs inside the OOM handler under the allocator's own lock: `format!` here would
    // re-enter the allocator. Keeps the `heap` prefix so existing greps still match.
    let mut note = [0u8; 21];
    note[..5].copy_from_slice(b"heap:");
    let sctx = get_sctx_id().raw() as u64;
    for i in 0..16 {
        let nib = ((sctx >> (60 - i * 4)) & 0xf) as u8;
        note[5 + i] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
    }
    let _ = sys_object_add_note(id, &note);

    if let Some(slot) = slot {
        Some((slot.slot, id))
    } else {
        release_object(id);
        None
    }
}

impl OomHandler for RuntimeOom {
    fn handle_oom(talc: &mut Talc<Self>, _layout: Layout) -> Result<(), ()> {
        // Arena growth cost (`OOMGROW`): object create + monitor map gate + delete-ctrl + note.
        // Once per ~1GB span, so for a fresh compartment this fires inside the first allocation
        // (mlibc's init_libc). Same switch as PREMAIN/CHILDINI.
        let _t0 = crate::runtime::core::PRE_MAIN_PHASE_STATS.then(std::time::Instant::now);
        let (slot, id) = create_and_map().ok_or(())?;
        if let Some(t0) = _t0 {
            secgate::statlog::record_on(true, "OOMGROW", t0.elapsed().as_micros() as u64, &[]);
        }
        // reserve an additional page size at the base of the object for future use. This behavior
        // may change as the runtime is fleshed out.
        const HEAP_OFFSET: usize = NULLPAGE_SIZE * 512;
        // offset from the endpoint of the object to where the endpoint of the heap is. Reserve a
        // page for the metadata + a few pages for any future FOT entries.
        const TOP_OFFSET: usize = NULLPAGE_SIZE * 4;
        let base = slot * MAX_SIZE + HEAP_OFFSET;
        let top = (slot + 1) * MAX_SIZE - TOP_OFFSET;
        // Hold the span's tail back from talc as the virgin region (see `RuntimeOom::virgin_next`).
        // A new span replaces the old region; whatever was left of it is abandoned (virtual space
        // in a span this allocator owns anyway, not committed memory).
        let talc_top = top - VIRGIN_RESERVE;

        unsafe {
            if talc
                .claim(Span::new(base as *mut _, talc_top as *mut _))
                .is_err()
            {
                release_object(id);
                return Err(());
            }
        }
        talc.oom_handler.virgin_next = talc_top;
        talc.oom_handler.virgin_top = top;

        if talc.oom_handler.list_obj.is_none() {
            talc.oom_handler.list_obj = Some(create_and_map().ok_or(())?);
            let slot = talc.oom_handler.list_obj.unwrap().0;
            let list_vec_start = slot * MAX_SIZE + HEAP_OFFSET;
            let list_vec_bytes = MAX_SIZE - TOP_OFFSET;
            let list_vec_cap = list_vec_bytes / size_of::<(usize, ObjID)>();
            let na = FailAlloc;
            talc.oom_handler.objects =
                unsafe { Vec::from_raw_parts_in(list_vec_start as *mut _, 0, list_vec_cap, na) };
        }

        talc.oom_handler.objects.push((slot, id));

        Ok(())
    }
}

struct FailAlloc;

unsafe impl Allocator for FailAlloc {
    fn allocate(&self, _layout: Layout) -> Result<NonNull<[u8]>, std::alloc::AllocError> {
        panic!("cannot allocate from FailAlloc. This is a bug.")
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        panic!("cannot allocate from FailAlloc. This is a bug.")
    }
}

unsafe impl GlobalAlloc for LocalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let layout =
            Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), MIN_ALIGN))
                .expect("layout alignment bump failed");
        let mut inner = self.inner.lock();
        let ptr = inner.do_alloc(layout);
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let layout =
            Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), MIN_ALIGN))
                .expect("layout alignment bump failed");

        // The monitor runtime has to deal with some weirdness in that some allocations may have
        // happened during bootstrap. It's possible that these could be freed into _this_
        // allocator, which would be wrong. So just ignore deallocations of bootstrap-allocated
        // memory.
        let ignore_slot = self.bootstrap_alloc_slot.load(Ordering::SeqCst);
        if ignore_slot != 0
            && Span::new(
                ((ignore_slot * MAX_SIZE) + NULLPAGE_SIZE) as *mut u8,
                ((ignore_slot * MAX_SIZE) + (MAX_SIZE - NULLPAGE_SIZE)) as *mut u8,
            )
            .contains(ptr)
        {
            return;
        }
        let mut inner = self.inner.lock();
        inner.do_dealloc(ptr, layout);
    }
}

impl LocalAllocator {
    pub fn alloc_early(&self, layout: Layout) -> *mut u8 {
        let layout =
            Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), MIN_ALIGN))
                .expect("layout alignment bump failed");
        let mut inner = self.inner.lock();
        let ptr = unsafe { inner.do_alloc_early(layout) };
        ptr
    }

    /// Free a pointer that came from [`Self::alloc_early`], back into the early talc.
    ///
    /// The monitor's allocations all take the early path (its runtime never reaches the allocator
    /// switch, so `early_allocs_frozen` stays false and `do_dealloc` drops its frees) — measured
    /// as ~360 B retained in the monitor heap per incoming gate call (leak25-floor, l0-stats10:
    /// 0.082 pages/call, r2 0.999). Every monitor pointer is an early_talc pointer, so freeing
    /// into early_talc is symmetric. Compartments never reach this: their early frees are dropped
    /// by the `is_ptr_early_alloc` gate before routing here, deliberately.
    pub fn dealloc_early(&self, ptr: *mut u8, layout: Layout) {
        let layout =
            Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), MIN_ALIGN))
                .expect("layout alignment bump failed");
        let mut inner = self.inner.lock();
        if let Some(ptr) = NonNull::new(ptr) {
            unsafe { inner.early_talc.free(ptr, layout) };
        }
    }

    pub fn alloc_zeroed_early(&self, layout: Layout) -> *mut u8 {
        let layout =
            Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), MIN_ALIGN))
                .expect("layout alignment bump failed");
        let _diag =
            crate::runtime::core::PRE_MAIN_PHASE_STATS && layout.size() >= EARLY_ALLOC_DIAG_MIN;
        let _t0 = _diag.then(std::time::Instant::now);
        let mut inner = self.inner.lock();

        // Serve large zeroed requests from the virgin region: guaranteed-zero, so no memset and no
        // first-touch sweep -- measured 109us per 512KB mlibc slab chunk otherwise. Monitor
        // excluded: `dealloc_early` frees into the early talc, which must never see a pointer talc
        // did not carve. Compartment early frees are dropped wholesale (`is_ptr_early_alloc` is
        // slot-based, and this region shares the span's slot), so no free path can reach talc.
        if layout.size() >= EARLY_ZERO_BUMP_MIN
            && !OUR_RUNTIME.state().contains(RuntimeState::IS_MONITOR)
        {
            let oh = &mut inner.early_talc.oom_handler;
            let next = oh.virgin_next.next_multiple_of(layout.align());
            if oh.virgin_top >= next + layout.size() {
                oh.virgin_next = next + layout.size();
                if let Some(t0) = _t0 {
                    secgate::statlog::record_on(
                        true,
                        "ALLCBIG",
                        t0.elapsed().as_micros() as u64,
                        &[layout.size() as u64, t0.elapsed().as_micros() as u64, 0],
                    );
                }
                return next as *mut u8;
            }
        }

        let ptr = unsafe { inner.do_alloc_early(layout) };
        let _t1 = _diag.then(std::time::Instant::now);
        unsafe { ptr.write_bytes(0, layout.size()) };
        // Large-early-allocation split (`ALLCBIG`): mlibc's slab pool maps 512KB chunks through
        // here during ctors (`spawnbench.md` §31); vals = [size, alloc_us, memset_us]. The memset
        // includes the first-touch faults on the fresh heap span.
        if let (Some(t0), Some(t1)) = (_t0, _t1) {
            secgate::statlog::record_on(
                true,
                "ALLCBIG",
                t0.elapsed().as_micros() as u64,
                &[
                    layout.size() as u64,
                    (t1 - t0).as_micros() as u64,
                    t1.elapsed().as_micros() as u64,
                ],
            );
        }
        ptr
    }
}

impl LocalAllocatorInner {
    const fn new() -> Self {
        Self {
            talc: Talc::new(RuntimeOom {
                objects: Vec::new_in(FailAlloc),
                list_obj: None,
                virgin_next: 0,
                virgin_top: 0,
            }),
            early_talc: Talc::new(RuntimeOom {
                objects: Vec::new_in(FailAlloc),
                list_obj: None,
                virgin_next: 0,
                virgin_top: 0,
            }),
            early_allocs_frozen: false,
        }
    }

    unsafe fn do_alloc(&mut self, layout: Layout) -> *mut u8 {
        if !self.early_allocs_frozen {
            return self.early_talc.malloc(layout).unwrap().as_ptr();
        }
        self.talc.malloc(layout).unwrap().as_ptr()
    }

    unsafe fn do_dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        if !self.early_allocs_frozen {
            return;
        }
        self.talc.free(NonNull::new(ptr).unwrap(), layout);
    }
    unsafe fn do_alloc_early(&mut self, layout: Layout) -> *mut u8 {
        self.early_talc.malloc(layout).unwrap().as_ptr()
    }
}

unsafe impl Allocator for LocalAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, std::alloc::AllocError> {
        let ptr = unsafe { self.alloc(layout) };
        if ptr.is_null() {
            Err(std::alloc::AllocError)
        } else {
            Ok(NonNull::slice_from_raw_parts(
                NonNull::new(ptr).unwrap(),
                layout.size(),
            ))
        }
    }

    unsafe fn deallocate(&self, ptr: std::ptr::NonNull<u8>, layout: Layout) {
        let ptr = ptr.as_ptr();
        unsafe { self.dealloc(ptr, layout) };
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, std::alloc::AllocError> {
        let ptr = unsafe { self.alloc_zeroed(layout) };
        if ptr.is_null() {
            Err(std::alloc::AllocError)
        } else {
            Ok(NonNull::slice_from_raw_parts(
                NonNull::new(ptr).unwrap(),
                layout.size(),
            ))
        }
    }
}
