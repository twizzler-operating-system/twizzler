use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use std::sync::Mutex;

use talc::{OomHandler, Span, Talc};
use twizzler_abi::{
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, sys_object_map, BackingType, LifetimeType, MapFlags, ObjectCreate,
        ObjectCreateFlags,
    },
};

use crate::{
    preinit_println,
    runtime::{slot::early_slot_alloc, RuntimeState},
};

use super::{ReferenceRuntime, OUR_RUNTIME};

static LOCAL_ALLOCATOR: LocalAllocator = LocalAllocator {
    runtime: &OUR_RUNTIME,
    early_lock: AtomicBool::new(false),
    early_alloc: UnsafeCell::new(Some(LocalAllocatorInner::new())),
    inner: Mutex::new(None),
};

unsafe impl Sync for LocalAllocator {}

impl ReferenceRuntime {
    pub fn get_alloc(&self) -> &'static LocalAllocator {
        &LOCAL_ALLOCATOR
    }
}

pub struct LocalAllocator {
    runtime: &'static ReferenceRuntime,
    early_lock: AtomicBool,
    early_alloc: UnsafeCell<Option<LocalAllocatorInner>>,
    inner: Mutex<Option<LocalAllocatorInner>>,
}

struct LocalAllocatorInner {
    talc: Talc<RuntimeOom>,
    //_objects: Vec<(usize, ObjID)>,
}

struct RuntimeOom {}

impl OomHandler for RuntimeOom {
    fn handle_oom(talc: &mut Talc<Self>, _layout: Layout) -> Result<(), ()> {
        preinit_println!("got OOM");
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
        .map_err(|_| ())?;

        let slot = early_slot_alloc().ok_or(())?;

        sys_object_map(
            None,
            id,
            slot,
            Protections::READ | Protections::WRITE,
            MapFlags::empty(),
        )
        .map_err(|_| ())?;

        let base = slot * MAX_SIZE + NULLPAGE_SIZE * 2;
        let top = (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 4;

        unsafe {
            talc.claim(Span::new(base as *mut _, top as *mut _))?;
        }

        // TODO: track the objects

        Ok(())
    }
}

unsafe impl GlobalAlloc for LocalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.runtime.state().contains(RuntimeState::READY) {
            // Runtime is ready, we can use normal locking
            let mut inner = self.inner.lock().unwrap();
            if inner.is_none() {
                // First ones in after bootstrap. Lock, and then grab the early_alloc, using it for ourselves.
                while !self.early_lock.swap(true, Ordering::SeqCst) {
                    core::hint::spin_loop()
                }
                assert!((*self.early_alloc.get()).is_some());
                *inner = (*self.early_alloc.get()).take();
                self.early_lock.store(false, Ordering::SeqCst);
            }
            inner.as_mut().unwrap().do_alloc(layout)
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
        if self.runtime.state().contains(RuntimeState::READY) {
            // Runtime is ready, we can use normal locking
            let mut inner = self.inner.lock().unwrap();
            if inner.is_none() {
                // First ones in after bootstrap. Lock, and then grab the early_alloc, using it for ourselves.
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
            talc: Talc::new(RuntimeOom {}),
            // objects: vec![],
        }
    }

    unsafe fn do_alloc(&mut self, layout: Layout) -> *mut u8 {
        self.talc.malloc(layout).unwrap().as_ptr()
    }

    unsafe fn do_dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        self.talc.free(NonNull::new(ptr).unwrap(), layout);
    }
}
