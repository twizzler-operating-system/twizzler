//! Rountines and definitions for the thread control block.
//!
//! Note that the control struct here uses a manual lock instead of a Mutex.
//! This is because the thread-control block may be accessed by libstd (or any
//! library, really, nearly arbitrarily, so we just avoid any complex code in here
//! that might call into std (with one exception, below).

use std::{
    alloc::{GlobalAlloc, Layout},
    collections::BTreeMap,
    panic::catch_unwind,
    sync::atomic::Ordering,
};

use dynlink::tls::Tcb;
use monitor_api::{RuntimeThreadControl, TlsTemplateInfo, THREAD_STARTED};
use twizzler_abi::simple_mutex::Mutex;

use crate::runtime::alloc::{LocalAllocator, LOCAL_ALLOCATOR};

/// Run a closure using the current thread's control struct, if this thread has a usable one.
///
/// The thread control block is found through the self-pointer at the thread pointer, so a thread
/// whose TLS has not been installed, or whose TLS region was freed and its memory reused, reads
/// that pointer as null. Returning `None` rather than unwrapping matters most for the allocator,
/// which calls this on every allocation: a panic there is unrecoverable, and the caller has a
/// correct fallback (treat the thread as not-yet-started and use the early allocator).
///
/// This cannot cover a thread pointer that is *itself* null -- reading `%fs:0` then faults rather
/// than yielding null, and nothing in userspace can test for it without asking the kernel.
pub(crate) fn try_with_current_thread<R, F: FnOnce(&RuntimeThreadControl) -> R>(f: F) -> Option<R> {
    let tp: &Tcb<RuntimeThreadControl> =
        unsafe { dynlink::tls::get_current_thread_control_block().as_ref()? };
    Some(f(&tp.runtime_data))
}

/// Run a closure using the current thread's control struct as the argument.
///
/// Panics if there is no usable control block. Use [`try_with_current_thread`] anywhere a panic is
/// worse than a degraded answer.
pub(crate) fn with_current_thread<R, F: FnOnce(&RuntimeThreadControl) -> R>(f: F) -> R {
    try_with_current_thread(f).expect("thread control block pointer is null")
}

// Entry point for threads.
pub(super) extern "C" fn trampoline(arg: usize) -> ! {
    // This is the same code used by libstd on catching a panic and turning it into an exit code.
    const THREAD_PANIC_CODE: u64 = 101;
    let code = catch_unwind(|| {
        // Indicate that we are alive.
        with_current_thread(|cur| {
            // Needs an acq barrier here for the ID, but also a release for the flags.
            cur.flags.fetch_or(THREAD_STARTED, Ordering::SeqCst);
        });
        // Find the arguments. arg is a pointer to a Box::into_raw of a Box of ThreadSpawnArgs.
        let arg = unsafe {
            (arg as *const twizzler_rt_abi::thread::ThreadSpawnArgs)
                .as_ref()
                .unwrap()
        };
        // Jump to the requested entry point. Handle the return, just in case, but this is
        // not supposed to return.
        let entry: extern "C" fn(usize) = unsafe { core::mem::transmute(arg.start) };
        (entry)(arg.arg);
        0
    })
    .unwrap_or(THREAD_PANIC_CODE);
    twizzler_abi::syscall::sys_thread_exit(code);
}

/// TLS regions from exited threads, waiting for the next spawn.
///
/// Same shape as the monitor's super-TLS pool and for the same reason: `SPAWNRT`'s tls phase is
/// 2.4 us median but 94 us mean with a 31 ms max (`sysperf.md` round 7, lead 4b). A 2-4 us median
/// is not worth a pool; a mean 40x the median is, and it is the same story as every other
/// allocation on this path -- a fresh span from the base allocator whose pages nothing has touched,
/// so the region's first write faults.
///
/// Two differences from the monitor's pool, both in this path's favor:
///
/// - **No re-zeroing.** [`TlsTemplateInfo::init_new_tls_region`] copies the whole `layout.size()`
///   prototype over the region and then rewrites the DTV and TCB, so every byte is defined
///   afterwards no matter what the allocation held. The monitor's path builds its region module by
///   module, copying only `template_filesz` each, which is why *it* has to zero.
/// - **No allocation and no `std` sync.** `get_next_tls_info` runs inside
///   `cross_compartment_entry`'s zero-thread-pointer window: a `Vec` here would allocate through
///   `ReferenceRuntime::alloc`, which reads the thread control block, and `std::sync::Mutex::lock`
///   consults `thread::panicking()`. Both read TLS. Hence a fixed array behind the same
///   `simple_mutex` that guards [`TLS_GEN_MGR`], with `take` doing nothing but a scan and a store.
///
/// The safety obligation is the one [`super::internal::InternalThread`]'s `Drop` already meets to
/// call `dealloc` there: the thread is gone, so nothing is running on this region. Worth naming one
/// consequence that differs from freeing, though: a recycled region is re-initialized with a valid
/// TCB whose `self_ptr` points at itself, so a stale thread pointer into it reads as a *plausible*
/// control block rather than as null. `with_current_thread`'s null check is correspondingly less
/// likely to catch a use-after-free of a thread pointer -- but `dealloc` hands the same memory to
/// the allocator for arbitrary reuse, which that check does not reliably catch either.
pub(super) mod tlspool {
    use std::alloc::Layout;

    use twizzler_abi::simple_mutex::Mutex;

    /// Regions held before further returns go back to the allocator.
    const MAX: usize = 8;

    /// A/B switch for measuring what recycling is worth; `false` restores allocating and freeing
    /// each region.
    const RECYCLE: bool = true;

    #[derive(Clone, Copy)]
    struct Entry {
        base: usize,
        size: usize,
        align: usize,
    }

    static POOL: Mutex<[Option<Entry>; MAX]> = Mutex::new([None; MAX]);

    /// A recycled region for exactly `layout`, if one is waiting.
    ///
    /// Keyed on the layout because it is per TLS generation: a region built for one generation is
    /// the wrong size for another, and matching on the layout is what keeps them apart.
    pub(in crate::runtime) fn take(layout: Layout) -> Option<*mut u8> {
        if !RECYCLE {
            return None;
        }
        let mut pool = POOL.lock();
        for slot in pool.iter_mut() {
            if let Some(e) = *slot {
                if e.size == layout.size() && e.align == layout.align() {
                    *slot = None;
                    return Some(e.base as *mut u8);
                }
            }
        }
        None
    }

    /// Returns false if the pool is full and the caller should free the region itself.
    pub(in crate::runtime) fn put(base: *mut u8, layout: Layout) -> bool {
        if !RECYCLE || base.is_null() {
            return false;
        }
        let mut pool = POOL.lock();
        for slot in pool.iter_mut() {
            if slot.is_none() {
                *slot = Some(Entry {
                    base: base as usize,
                    size: layout.size(),
                    align: layout.align(),
                });
                return true;
            }
        }
        false
    }
}

pub(crate) struct TlsGenMgr {
    /// Deliberately allocator-parameterized: `get_next_tls_info` runs inside
    /// `cross_compartment_entry`'s window, where the thread pointer is zero, and a `BTreeMap` on
    /// the global allocator would allocate a node through `ReferenceRuntime::alloc` -- which
    /// reads the thread control block and would fault. `LocalAllocator` reaches talc directly
    /// and touches no thread-local state.
    map: BTreeMap<u64, TlsGen, &'static LocalAllocator>,
}

pub(crate) struct TlsGen {
    template: TlsTemplateInfo,
    thread_count: usize,
}

unsafe impl Send for TlsGen {}

pub(crate) static TLS_GEN_MGR: Mutex<TlsGenMgr> = Mutex::new(TlsGenMgr {
    map: BTreeMap::new_in(&LOCAL_ALLOCATOR),
});

impl TlsGenMgr {
    pub fn _need_new_gen(&self, mygen: Option<u64>) -> bool {
        let cc = monitor_api::get_comp_config();
        let template = unsafe { cc.get_tls_template().as_ref().unwrap() };
        mygen.is_some_and(|mygen| mygen == template.gen)
    }

    pub fn get_next_tls_info<T>(
        &mut self,
        mygen: Option<u64>,
        new_tcb_data: impl FnOnce() -> T,
    ) -> Option<(*mut Tcb<T>, Layout, *mut u8)> {
        let cc = monitor_api::get_comp_config();
        let template = unsafe { cc.get_tls_template().as_ref().unwrap() };
        if mygen.is_some_and(|mygen| mygen == template.gen) {
            return None;
        }

        let new = tlspool::take(template.layout)
            .unwrap_or_else(|| unsafe { LOCAL_ALLOCATOR.alloc(template.layout) });
        let tlsgen = self.map.entry(template.gen).or_insert_with(|| TlsGen {
            template: *template,
            thread_count: 0,
        });
        tlsgen.thread_count += 1;

        unsafe {
            let tcb = tlsgen.template.init_new_tls_region(new, new_tcb_data());

            Some((tcb, template.layout, new))
        }
    }

    // TODO: when threads exit or move on to a different TLS gen, track that in thread_count, and if
    // it hits zero, notify the monitor.
}

/// Bring the current thread's DTV up to the compartment's current TLS template.
///
/// Called from the `__tls_get_addr` slow path when a module ID lands beyond this thread's DTV:
/// a library with a PT_TLS segment was loaded after this thread's region was built, and the
/// monitor republished the template with the new generation.
///
/// The thread's existing region is untouched -- DTV entries for modules it already has keep
/// pointing into it, so live thread-locals keep their values and addresses. Blocks for the new
/// modules come from a fresh copy of the template's prototype region (which also materializes
/// blocks for the old modules; those go unused -- the price of not shipping per-module layout
/// info across the monitor boundary). A new, larger DTV replaces the fixed-size one inside the
/// thread's region.
///
/// The replaced DTV and, on thread exit, the appendix region and DTV allocation are leaked;
/// reclaiming them is part of TLS generation retirement (tracked in the TODO above). Note this
/// only serves general-dynamic accesses: initial-exec and TLSDESC relocations against a
/// runtime-loaded module resolve to static offsets that only threads built from the new
/// template have.
///
/// Returns false if the thread is already at (or beyond) the current template, meaning the
/// caller's lookup failure was a genuinely bad module ID.
pub(crate) fn upgrade_current_thread_dtv() -> bool {
    let cc = monitor_api::get_comp_config();
    let template = unsafe { cc.get_tls_template().as_ref().unwrap() };

    let tcb: *mut Tcb<()> = unsafe { dynlink::tls::get_current_thread_control_block() };
    if tcb.is_null() {
        return false;
    }
    let (old_dtv, old_len) = unsafe { ((*tcb).dtv, (*tcb).dtv_len) };
    let new_len = template.num_dtv_entries;
    if new_len <= old_len {
        return false;
    }

    unsafe {
        let region = LOCAL_ALLOCATOR.alloc(template.layout);
        if region.is_null() {
            return false;
        }
        core::ptr::copy_nonoverlapping(
            template.alloc_base.as_ptr(),
            region,
            template.layout.size(),
        );

        let dtv_layout = Layout::array::<usize>(new_len).unwrap();
        let new_dtv = LOCAL_ALLOCATOR.alloc(dtv_layout) as *mut usize;
        if new_dtv.is_null() {
            LOCAL_ALLOCATOR.dealloc(region, template.layout);
            return false;
        }

        // dtv[0] is the generation count; old modules keep their blocks; new modules point into
        // the fresh region copy at the offsets the prototype's own DTV records.
        *new_dtv = template.gen as usize;
        core::ptr::copy_nonoverlapping(old_dtv.add(1), new_dtv.add(1), old_len - 1);
        let proto_dtv = template.alloc_base.as_ptr().add(template.dtv_offset) as *const usize;
        for i in old_len..new_len {
            let offset = *proto_dtv.add(i) - template.alloc_base.as_ptr() as usize;
            *new_dtv.add(i) = region as usize + offset;
        }

        // Only this thread reads its own dtv/dtv_len, so plain stores publish safely.
        (*tcb).dtv = new_dtv;
        (*tcb).dtv_len = new_len;
    }
    true
}

extern "C" {
    #[linkage = "extern_weak"]
    static __mlibc_init_tcb: *mut u8;
}

pub(crate) fn libc_init_tcb<T>(tcb: *mut Tcb<T>) {
    unsafe {
        if !__mlibc_init_tcb.is_null() {
            let mlibc_init_tcb =
                std::mem::transmute::<_, extern "C" fn(*mut Tcb<T>)>(__mlibc_init_tcb);
            mlibc_init_tcb(tcb);
        }
    }
}
