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

struct LocalAllocatorInner {
    talc: Talc<RuntimeOom>,
    early_talc: Talc<RuntimeOom>,
    early_allocs_frozen: bool,
}

struct RuntimeOom {
    list_obj: Option<(usize, ObjID)>,
    objects: Vec<(usize, ObjID), FailAlloc>,
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
    let _ = sys_object_add_note(id, b"heap");

    if let Some(slot) = slot {
        Some((slot.slot, id))
    } else {
        release_object(id);
        None
    }
}

impl OomHandler for RuntimeOom {
    fn handle_oom(talc: &mut Talc<Self>, _layout: Layout) -> Result<(), ()> {
        let (slot, id) = create_and_map().ok_or(())?;
        // reserve an additional page size at the base of the object for future use. This behavior
        // may change as the runtime is fleshed out.
        const HEAP_OFFSET: usize = NULLPAGE_SIZE * 512;
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
                release_object(id);
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

    pub fn alloc_zeroed_early(&self, layout: Layout) -> *mut u8 {
        let layout =
            Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), MIN_ALIGN))
                .expect("layout alignment bump failed");
        let mut inner = self.inner.lock();
        let ptr = unsafe { inner.do_alloc_early(layout) };
        unsafe { ptr.write_bytes(0, layout.size()) };
        ptr
    }
}

impl LocalAllocatorInner {
    const fn new() -> Self {
        Self {
            talc: Talc::new(RuntimeOom {
                objects: Vec::new_in(FailAlloc),
                list_obj: None,
            }),
            early_talc: Talc::new(RuntimeOom {
                objects: Vec::new_in(FailAlloc),
                list_obj: None,
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
