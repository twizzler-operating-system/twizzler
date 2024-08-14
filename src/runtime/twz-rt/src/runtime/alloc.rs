//! Primary allocator, for compartment-local allocation. One tricky aspect to this is that we need
//! to support allocation before the runtime is fully ready, so to avoid calling into std, we
//! implement a manual spinlock around the allocator until the better Mutex is available. Once it
//! is, we move the allocator into the mutex, and use that.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{
    alloc::Allocator,
    mem::size_of,
    sync::{atomic::AtomicUsize, Mutex},
};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const MIN_ALIGN: usize = 16;

use talc::{OomHandler, Span, Talc};
use tracing::warn;
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_runtime_api::MapFlags;

use super::{ReferenceRuntime, OUR_RUNTIME};
use crate::{preinit_println, runtime::RuntimeState};

static LOCAL_ALLOCATOR: LocalAllocator = LocalAllocator {
    runtime: &OUR_RUNTIME,
    early_lock: AtomicBool::new(false),
    early_alloc: UnsafeCell::new(Some(LocalAllocatorInner::new())),
    inner: Mutex::new(None),
    bootstrap_alloc_slot: AtomicUsize::new(0),
};

unsafe impl Sync for LocalAllocator {}

impl ReferenceRuntime {
    pub fn get_alloc(&self) -> &'static LocalAllocator {
        &LOCAL_ALLOCATOR
    }

    pub(crate) fn register_bootstrap_alloc(&self, slot: usize) {
        LOCAL_ALLOCATOR
            .bootstrap_alloc_slot
            .store(slot, Ordering::SeqCst);
    }
}

pub struct LocalAllocator {
    runtime: &'static ReferenceRuntime,
    // early allocation need a lock, but mutex isn't usable yet.
    early_lock: AtomicBool,
    early_alloc: UnsafeCell<Option<LocalAllocatorInner>>,
    inner: Mutex<Option<LocalAllocatorInner>>,
    bootstrap_alloc_slot: AtomicUsize,
}

struct LocalAllocatorInner {
    talc: Talc<RuntimeOom>,
}

struct RuntimeOom {
    list_obj: Option<(usize, ObjID)>,
    objects: Vec<(usize, ObjID), FailAlloc>,
}

fn delete_obj(_id: ObjID) {
    // TODO
    warn!("unimplemented: delete object due to failure in allocator");
}

fn create_and_map() -> Option<(usize, ObjID)> {
    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        ),
        &[],
        &[],
    )
    .ok()?;

    let slot = monitor_api::monitor_rt_object_map(id, MapFlags::READ | MapFlags::WRITE)
        .unwrap()
        .ok();

    if let Some(slot) = slot {
        Some((slot.slot, id))
    } else {
        delete_obj(id);
        None
    }
}

impl OomHandler for RuntimeOom {
    fn handle_oom(talc: &mut Talc<Self>, _layout: Layout) -> Result<(), ()> {
        let (slot, id) = create_and_map().ok_or(())?;
        // reserve an additional page size at the base of the object for future use. This behavior
        // may change as the runtime is fleshed out.
        const HEAP_OFFSET: usize = NULLPAGE_SIZE * 2;
        // offset from the endpoint of the object to where the endpoint of the heap is. Reserve a
        // page for the metadata + a few pages for any future FOT entries.
        const TOP_OFFSET: usize = NULLPAGE_SIZE * 4;
        let base = slot * MAX_SIZE + HEAP_OFFSET;
        let top = (slot + 1) * MAX_SIZE - TOP_OFFSET;

        unsafe {
            if talc
                .claim(Span::new(base as *mut _, top as *mut _))
                .is_err()
            {
                delete_obj(id);
                monitor_api::monitor_rt_object_unmap(slot).unwrap();
                return Err(());
            }
        }

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
        if self.runtime.state().contains(RuntimeState::READY) {
            // Runtime is ready, we can use normal locking
            let mut inner = self.inner.lock().unwrap();
            if inner.is_none() {
                // First ones in after bootstrap. Lock, and then grab the early_alloc, using it for
                // ourselves.
                while !self.early_lock.swap(true, Ordering::SeqCst) {
                    core::hint::spin_loop()
                }
                assert!((*self.early_alloc.get()).is_some());
                *inner = (*self.early_alloc.get()).take();
                self.early_lock.store(false, Ordering::SeqCst);
            }

            let ptr = inner.as_mut().unwrap().do_alloc(layout);
            ptr
        } else {
            // Runtime is NOT ready. Use a basic spinlock to prevent calls to std.
            while !self.early_lock.swap(true, Ordering::SeqCst) {
                core::hint::spin_loop()
            }
            assert!((*self.early_alloc.get()).is_some());
            let ret = self
                .early_alloc
                .get()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .do_alloc(layout);
            self.early_lock.store(false, Ordering::SeqCst);
            ret
        }
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

        if self.runtime.state().contains(RuntimeState::READY) {
            // Runtime is ready, we can use normal locking
            let mut inner = self.inner.lock().unwrap();
            if inner.is_none() {
                // First ones in after bootstrap. Lock, and then grab the early_alloc, using it for
                // ourselves.
                while !self.early_lock.swap(true, Ordering::SeqCst) {
                    core::hint::spin_loop()
                }
                assert!((*self.early_alloc.get()).is_some());
                *inner = (*self.early_alloc.get()).take();
                self.early_lock.store(false, Ordering::SeqCst);
            }

            inner.as_mut().unwrap().do_dealloc(ptr, layout);
        } else {
            // Runtime is NOT ready. Use a basic spinlock to prevent calls to std.
            while !self.early_lock.swap(true, Ordering::SeqCst) {
                core::hint::spin_loop()
            }
            assert!((*self.early_alloc.get()).is_some());
            self.early_alloc
                .get()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .do_dealloc(ptr, layout);
            self.early_lock.store(false, Ordering::SeqCst);
        }
    }
}

impl LocalAllocatorInner {
    const fn new() -> Self {
        Self {
            talc: Talc::new(RuntimeOom {
                objects: Vec::new_in(FailAlloc),
                list_obj: None,
            }),
        }
    }

    unsafe fn do_alloc(&mut self, layout: Layout) -> *mut u8 {
        self.talc.malloc(layout).unwrap().as_ptr()
    }

    unsafe fn do_dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        self.talc.free(NonNull::new(ptr).unwrap(), layout);
    }
}
